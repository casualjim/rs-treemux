//! [`Treemux`](crate::Treemux) is a lightweight high performance HTTP request router.
//!
//! This router supports variables in the routing pattern and matches against
//! the request method. It also scales better.
//!
//! The router is optimized for high performance and a small memory footprint.
//! It scales well even with very long paths and a large number of routes.
//! A compressing dynamic trie (radix tree) structure is used for efficient matching.
//!
//!
//! ```rust,no_run
//! use treemux::{Treemux, RouterBuilder, Params};
//! use std::convert::Infallible;
//! use hyper::{Request, Response, Body};
//! use hyper::http::Error;
//!
//! async fn index(_: Request<Body>) -> Result<Response<Body>, Error> {
//!     Ok(Response::new("Hello, World!".into()))
//! }
//!
//! async fn hello(req: Request<Body>) -> Result<Response<Body>, Error> {
//!     let params = req.extensions().get::<Params>().unwrap();
//!     Ok(Response::new(format!("Hello, {}", params.get("user").unwrap()).into()))
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let mut router = Treemux::builder();
//!     router.get("/", index);
//!     router.get("/hello/:user", hello);
//!
//!     hyper::Server::bind(&([127, 0, 0, 1], 3000).into())
//!         .serve(router.into_service())
//!         .await;
//! }
//!```
//!
//! The registered path, against which the router matches incoming requests, can
//! contain two types of parameters:
//! ```ignore
//!  Syntax    Type
//!  :name     named parameter
//!  *name     catch-all parameter
//! ```
//!
//! Named parameters are dynamic path segments. They match anything until the
//! next '/' or the path end:
//! ```ignore
//!  Path: /blog/:category/:post
//! ```
//!
//!  Requests:
//! ```ignore
//!   /blog/rust/request-routers            match: category="rust", post="request-routers"
//!   /blog/rust/request-routers/           no match, but the router would redirect
//!   /blog/rust/                           no match
//!   /blog/rust/request-routers/comments   no match
//! ```
//!
//! Catch-all parameters match anything until the path end, including the
//! directory index (the '/' before the catch-all). Since they match anything
//! until the end, catch-all parameters must always be the final path element.
//!  Path: /files/*filepath
//!
//!  Requests:
//! ```ignore
//!   /files/                             match: filepath="/"
//!   /files/LICENSE                      match: filepath="/LICENSE"
//!   /files/templates/article.html       match: filepath="/templates/article.html"
//!   /files                              no match, but the router would redirect
//! ```
//! The value of parameters is saved as a `Vec` of the `Param` struct, consisting
//! each of a key and a value.
//!
//! There are two ways to retrieve the value of a parameter:
//!  1) by the name of the parameter
//! ```rust,no_run
//!  # use treemux::Params;
//!  # let params = Params::default();
//!
//!  let user = params.get("user"); // defined by :user or *user
//! ```
//!  2) by the index of the parameter. This way you can also get the name (key)
//! ```rust,no_run
//!  # use treemux::Params;
//!  # let params = Params::default();
//!  let third_key = &params[2].key;   // the name of the 3rd parameter
//!  let third_value = &params[2].value; // the value of the 3rd parameter
//! ```

use crate::{
  serve::MakeRouterService,
  tree::{HandlerConfig, Match, Node, Params},
};

use futures::FutureExt;
use header::HeaderValue;
use hyper::{
  header::{self, CONTENT_TYPE, LOCATION},
  http, Body, Method, Request, Response, StatusCode,
};
use std::{borrow::Cow, collections::HashMap, fmt::Display, panic::AssertUnwindSafe};
use std::{
  fmt::Debug,
  sync::{Arc, Mutex},
};
use std::{future::Future, net::SocketAddr};
use std::{pin::Pin, str};

pub trait RequestExt {
  fn params(&self) -> Option<&Params>;
  fn remote_addr(&self) -> SocketAddr;
  fn app_context<T: Send + Sync + 'static>(&self) -> Option<Arc<T>>;
}

impl RequestExt for Request<Body> {
  fn params(&self) -> Option<&Params> {
    self.extensions().get::<Params>()
  }

  fn remote_addr(&self) -> SocketAddr {
    self
      .extensions()
      .get::<SocketAddr>()
      .copied()
      .expect("No remote address present on the request")
  }

  fn app_context<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
    self.extensions().get::<Arc<T>>().cloned()
  }
}

pub trait Middleware: Send + Sync + 'static {
  type Input;
  type Output;

  fn chain(&self, input: Self::Input) -> Self::Output;
}

impl<F> Middleware for F
where
  F: Fn(Handler) -> Handler + Send + Sync + 'static,
{
  type Input = Handler;

  type Output = Handler;

  fn chain(&self, input: Self::Input) -> Self::Output {
    self(input)
  }
}

pub type Handler = Box<dyn Fn(Request<Body>) -> HandlerReturn + Send + Sync + 'static>;
pub type HandlerReturn = Pin<Box<dyn Future<Output = Result<Response<Body>, http::Error>> + Send + 'static>>;
pub type MethodNotAllowedHandler = Box<dyn Fn(Request<Body>, Vec<Method>) -> HandlerReturn + Send + Sync + 'static>;
pub type PanicHandler = Box<dyn Fn(HandlerReturn) -> HandlerReturn + Send + Sync + 'static>;

#[derive(Clone)]
struct Route {
  pattern: String,
  handler: Arc<Handler>,
}

impl std::fmt::Debug for Route {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Route")
      .field("pattern", &self.pattern)
      .field("handler", &"{{closure}}".to_owned())
      .finish()
  }
}

impl Route {
  fn new<P, H, R>(pattern: P, handler: H) -> Route
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    let handler: Handler = Box::new(move |req: Request<hyper::Body>| Box::pin(handler(req)));
    Route {
      pattern: pattern.into(),
      handler: Arc::new(handler),
    }
  }
}

pub struct GroupBuilder<'a> {
  prefix: Cow<'a, str>,
  inner: &'a Builder,
  chain: Arc<dyn Middleware<Input = Handler, Output = Handler>>,
}

impl<'a> GroupBuilder<'a> {
  pub fn extend<P: Into<Cow<'static, str>>, B: Into<Treemux>>(&self, path: P, routes: B) {
    let p: Cow<str> = path.into();
    if p.is_empty() {
      panic!("Scope path must not be empty");
    }

    check_path(p.clone());
    let pth = format!("{}{}", self.prefix, p.as_ref());
    self.inner.root.lock().unwrap().extend(
      (&pth).strip_suffix('/').unwrap_or(&pth).to_string().into(),
      routes.into().root,
    )
  }

  pub fn scope<'b, P: Into<Cow<'static, str>>>(&'b mut self, path: P) -> GroupBuilder<'b> {
    let p: Cow<str> = path.into();
    if p.is_empty() {
      panic!("Scope path must not be empty");
    }

    check_path(p.clone());
    let pth = format!("{}{}", self.prefix, p.as_ref());

    GroupBuilder {
      prefix: (&pth).strip_suffix('/').unwrap_or(&pth).to_string().into(),
      inner: self.inner,
      chain: self.chain.clone(),
    }
  }

  pub fn middleware<M>(&mut self, middleware: M)
  where
    M: Middleware<Input = Handler, Output = Handler>,
  {
    let previous = self.chain.clone();
    self.chain = Arc::new(move |handler| previous.chain(middleware.chain(handler)));
  }
}

impl<'a> RouterBuilder for GroupBuilder<'a> {
  fn handle<P, H, R>(&self, method: Method, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    let path: Cow<str> = path.into().into();
    let path: Cow<str> = format!("{}{}", self.prefix, path).into();
    check_path(path.clone());

    self.inner.add_route(method, path, handler)
  }
}

async fn default_not_found() -> Result<Response<Body>, http::Error> {
  Response::builder()
    .status(StatusCode::NOT_FOUND)
    .body(Body::from(format!("{}", StatusCode::NOT_FOUND)))
    .map_err(Into::into)
}

async fn default_method_not_allowed(allow: Vec<Method>) -> Result<Response<Body>, http::Error> {
  Response::builder()
    .status(StatusCode::METHOD_NOT_ALLOWED)
    .header(
      header::ALLOW,
      allow
        .iter()
        .map(|v| v.as_str().to_string())
        .collect::<Vec<String>>()
        .join(", "),
    )
    .body(Body::from(format!("{}", StatusCode::METHOD_NOT_ALLOWED)))
    .map_err(Into::into)
}

async fn handle_panics(
  fut: impl Future<Output = Result<Response<Body>, http::Error>>,
) -> Result<Response<Body>, http::Error> {
  let wrapped = AssertUnwindSafe(fut).catch_unwind();
  match wrapped.await {
    Ok(response) => response,
    Err(panic) => {
      let error = Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(Body::from(format!("{:?}", panic)))
        .unwrap();
      Ok(error)
    }
  }
}

pub struct Builder {
  path: Cow<'static, str>,
  root: Arc<Mutex<Node<'static, Route>>>,
  chain: Arc<dyn Middleware<Input = Handler, Output = Handler>>,
  handle_not_found: Option<Handler>,
  handle_method_not_allowed: Option<MethodNotAllowedHandler>,
  handle_global_options: Option<Handler>,
  handle_panic: Option<PanicHandler>,
  /// Allows the router to use the `GET` handler to respond to
  /// `HEAD` requests if no explicit `HEAD` handler has been added for the
  /// matching pattern. This is true by default.
  pub head_can_use_get: bool,

  /// Enables automatic redirection in case the router doesn't find a matching route
  /// for the current request path but a handler for the path with or without the trailing
  /// slash exists. This is true by default.
  pub redirect_trailing_slash: bool,

  /// Allows the router to try clean the current request path,
  /// if no handler is registered for it.This is true by default.
  pub redirect_clean_path: bool,

  /// Overrides the default behavior for a particular HTTP method.
  /// The key is the method name, and the value is the behavior to use for that method.
  pub redirect_behavior: Option<StatusCode>,

  /// Overrides the default behavior for a particular HTTP method.
  /// The key is the method name, and the value is the behavior to use for that method.
  pub redirect_method_behavior: HashMap<Method, StatusCode>,

  /// Removes the trailing slash when a catch-all pattern
  /// is matched, if set to true. By default, catch-all paths are never redirected.
  pub remove_catach_all_trailing_slash: bool,

  /// Controls URI escaping behavior when adding a route to the tree.
  /// If set to true, the router will add both the route as originally passed, and
  /// a version passed through url::parse. This behavior is disabled by default.
  pub escape_added_routes: bool,
}

impl Default for Builder {
  fn default() -> Self {
    Self {
      path: "".into(),
      root: Arc::new(Mutex::new(Node::new())),
      chain: Arc::new(|handler| handler),
      handle_not_found: None,
      handle_method_not_allowed: None,
      handle_global_options: None,
      handle_panic: None,
      head_can_use_get: true,
      redirect_trailing_slash: true,
      redirect_clean_path: true,
      redirect_behavior: Some(StatusCode::MOVED_PERMANENTLY),
      redirect_method_behavior: HashMap::default(),
      remove_catach_all_trailing_slash: false,
      escape_added_routes: false,
    }
  }
}

impl Builder {
  pub fn extend<P: Into<Cow<'static, str>>, B: Into<Treemux>>(&self, path: P, routes: B) {
    let p: Cow<str> = path.into();
    if p.is_empty() {
      panic!("Scope path must not be empty");
    }

    check_path(p.clone());
    let pth = format!("{}{}", self.path, p.as_ref());
    self.root.lock().unwrap().extend(
      (&pth).strip_suffix('/').unwrap_or(&pth).to_string().into(),
      routes.into().root,
    )
  }

  pub fn scope<'b, P: Into<Cow<'b, str>>>(&'b mut self, path: P) -> GroupBuilder<'b> {
    let p: Cow<str> = path.into();
    if p.is_empty() {
      panic!("Scope path must not be empty");
    }

    check_path(p.clone());
    let pth = format!("{}{}", self.path, p.as_ref());
    GroupBuilder {
      prefix: (&pth).strip_suffix('/').unwrap_or(&pth).to_string().into(),
      inner: self,
      chain: self.chain.clone(),
    }
  }

  pub fn middleware<M>(&mut self, middleware: M)
  where
    M: Middleware<Input = Handler, Output = Handler>,
  {
    let previous = self.chain.clone();
    self.chain = Arc::new(move |handler| previous.chain(middleware.chain(handler)));
  }

  /// Register a handler for when there is no match
  pub fn not_found<H, R>(&mut self, handler: H)
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    let req_handler = self.chain.clone().chain(Box::new(move |req| Box::pin(handler(req))));
    self.handle_not_found = Some(req_handler);
  }

  /// Register a handler for when the path matches a different method than the requested one
  pub fn method_not_allowed<H, R>(&mut self, handler: H)
  where
    H: Fn(Request<Body>, Vec<Method>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    let chain = self.chain.clone();
    let handler = Arc::new(handler);
    let req_handler: MethodNotAllowedHandler = Box::new(move |req, allowed| {
      let handler = handler.clone();
      Box::pin(chain
        .clone()
        .chain(Box::new(move |rr| Box::pin(handler(rr, allowed.clone()))))(
        req
      ))
    });
    self.handle_method_not_allowed = Some(req_handler);
  }

  /// Register a handler for when the path matches a different method than the requested one
  pub fn global_options<H, R>(&mut self, handler: H)
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    let req_handler = self.chain.clone().chain(Box::new(move |req| Box::pin(handler(req))));
    self.handle_global_options = Some(req_handler);
  }

  pub fn handle_panics<H, R>(&mut self, handler: H)
  where
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
    H: Fn(HandlerReturn) -> R + Send + Sync + 'static,
  {
    let ph: PanicHandler = Box::new(move |fut| Box::pin(handler(fut)));
    self.handle_panic = Some(ph);
  }

  fn build(self) -> Treemux {
    let root = Arc::try_unwrap(self.root)
      .map_err(|_| ())
      .unwrap()
      .into_inner()
      .unwrap();
    let mut result = Treemux::new(root);
    result.handle_not_found = self.handle_not_found.map(Arc::new);
    result.handle_method_not_allowed = self.handle_method_not_allowed.map(Arc::new);
    result.handle_global_options = self.handle_global_options.map(Arc::new);
    result.handle_panic = self.handle_panic;
    result.head_can_use_get = self.head_can_use_get;
    result.redirect_trailing_slash = self.redirect_trailing_slash;
    result.redirect_clean_path = self.redirect_clean_path;
    result.redirect_behavior = self.redirect_behavior;
    result.redirect_method_behavior = self.redirect_method_behavior;
    result.remove_catch_all_trailing_slash = self.remove_catach_all_trailing_slash;
    result.escape_added_routes = self.escape_added_routes;
    result
  }

  /// Converts the `Treemux` into a `Service` which you can serve directly with `Hyper`.
  /// If you have an existing `Service` that you want to incorporate a `Treemux` into, see
  /// [`Treemux::serve`](crate::Treemux::serve).
  /// ```rust,no_run
  /// # use treemux::{Treemux, RouterBuilder};
  /// # use std::convert::Infallible;
  /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
  /// // Our router...
  /// let router = Treemux::builder();
  ///
  /// // Convert it into a service...
  /// let service = router.into_service();
  ///
  /// // Serve with hyper
  /// hyper::Server::bind(&([127, 0, 0, 1], 3030).into())
  ///     .serve(service)
  ///     .await?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn into_service_with_context<T: Send + Sync + 'static>(self, context: T) -> MakeRouterService<T> {
    MakeRouterService(Arc::new(context), self.build())
  }

  /// Converts the `Treemux` into a `Service` which you can serve directly with `Hyper`.
  /// If you have an existing `Service` that you want to incorporate a `Treemux` into, see
  /// [`Treemux::serve`](crate::Treemux::serve).
  /// ```rust,no_run
  /// # use treemux::{Treemux, RouterBuilder};
  /// # use std::convert::Infallible;
  /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
  /// // Our router...
  /// let router = Treemux::builder();
  ///
  /// // Convert it into a service...
  /// let service = router.into_service();
  ///
  /// // Serve with hyper
  /// hyper::Server::bind(&([127, 0, 0, 1], 3030).into())
  ///     .serve(service)
  ///     .await?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn into_service(self) -> MakeRouterService<()> {
    MakeRouterService(Arc::new(()), self.build())
  }

  fn add_route<H, R>(&self, method: Method, path: Cow<'static, str>, handler: H)
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    if path.is_empty() {
      panic!("Cannot map an empty path");
    }

    let mut root = self.root.lock().unwrap();
    let (add_slash, path) = if path.len() > 1 && path.ends_with('/') && self.redirect_trailing_slash {
      (true, path.strip_suffix('/').unwrap_or_default().to_string().into())
    } else {
      (false, path)
    };

    let req_handler = self.chain.clone().chain(Box::new(move |req| Box::pin(handler(req))));
    let route = Route::new(path.clone(), req_handler);
    let mut hcfg = HandlerConfig::new(method.clone(), path.clone(), route.clone());
    hcfg.add_slash = add_slash;
    hcfg.head_can_use_get = self.head_can_use_get;
    root.insert(hcfg);

    if self.escape_added_routes {
      let u: http::Uri = path.clone().parse().unwrap();
      let escaped_path: Cow<str> = u.path().to_owned().into();
      if escaped_path != path {
        let mut hcfg = HandlerConfig::new(
          method,
          escaped_path.clone(),
          Route {
            pattern: escaped_path.to_string(),
            handler: route.handler.clone(),
          },
        );
        hcfg.add_slash = add_slash;
        hcfg.head_can_use_get = self.head_can_use_get;
        root.insert(hcfg);
      }
    }
  }
}

pub fn check_path(path: Cow<str>) {
  if !path.starts_with('/') {
    panic!("Path {} must start with a slash", path);
  }
}

impl RouterBuilder for Builder {
  fn handle<P, H, R>(&self, method: Method, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    let path: Cow<str> = path.into().into();
    check_path(path.clone());
    let path: Cow<str> = format!("{}{}", self.path, path).into();
    self.add_route(method, path, handler)
  }
}

pub struct Treemux {
  root: Node<'static, Route>,
  handle_not_found: Option<Arc<Handler>>,
  handle_method_not_allowed: Option<Arc<MethodNotAllowedHandler>>,
  handle_global_options: Option<Arc<Handler>>,
  handle_panic: Option<PanicHandler>,
  head_can_use_get: bool,
  redirect_trailing_slash: bool,
  redirect_clean_path: bool,
  redirect_behavior: Option<StatusCode>,
  redirect_method_behavior: HashMap<Method, StatusCode>,
  remove_catch_all_trailing_slash: bool,
  escape_added_routes: bool,
}

impl Debug for Treemux {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Treemux")
      .field("handle_not_found", &self.handle_not_found.is_some())
      .field("handle_method_not_allowed", &self.handle_method_not_allowed.is_some())
      .field("handle_global_options", &self.handle_global_options.is_some())
      .field("handle_panics", &self.handle_panic.is_some())
      .field("head_can_use_get", &self.head_can_use_get)
      .field("redirect_trailing_slash", &self.redirect_trailing_slash)
      .field("redirect_clean_path", &self.redirect_clean_path)
      .field("redirect_behavior", &self.redirect_behavior)
      .field("redirect_method_behavior", &self.redirect_method_behavior)
      .field("remove_catch_all_trailing_slash", &self.remove_catch_all_trailing_slash)
      .field("escape_added_routes", &self.escape_added_routes)
      .field("routes", &self.root.dump_tree("", ""))
      .finish()
  }
}

impl Display for Treemux {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.root.dump_tree("", ""))
  }
}

impl Treemux {
  fn new(root: Node<'static, Route>) -> Self {
    Self {
      root,
      ..Default::default()
    }
  }

  pub fn builder() -> Builder {
    Builder::default()
  }

  /// Lookup allows the manual lookup of handler for a specific method and path.
  /// If the handler is not found, it returns a `Err(bool)` indicating whether a redirection should be performed to the same path with a trailing slash
  /// ```rust
  /// use treemux::{Treemux, RouterBuilder};
  /// use hyper::{Response, Body, Method};
  ///
  /// let mut router = Treemux::builder();
  /// router.get("/home", |_| {
  ///     async { Ok(Response::new(Body::from("Welcome!"))) }
  /// });
  /// let router: Treemux = router.into();
  ///
  /// let res = router.lookup(&Method::GET, "/home").unwrap();
  /// assert!(res.1.is_empty());
  /// ```
  pub fn lookup<'b, P: AsRef<str>>(&'b self, method: &'b Method, path: P) -> Result<(Arc<Handler>, Params), bool> {
    self
      .root
      .search(method, path)
      .map(|m| {
        let vv = m.value.unwrap();
        Ok((vv.handler.clone(), m.parameters.clone()))
      })
      .unwrap_or(Err(false))
  }

  /// Converts the `Treemux` into a `Service` which you can serve directly with `Hyper`.
  /// If you have an existing `Service` that you want to incorporate a `Treemux` into, see
  /// [`Treemux::serve`](crate::Treemux::serve).
  /// ```rust,no_run
  /// # use treemux::{Treemux, RouterBuilder};
  /// # use std::convert::Infallible;
  /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
  /// // Our router...
  /// let router = Treemux::builder();
  ///
  /// // Convert it into a service...
  /// let service = router.into_service();
  ///
  /// // Serve with hyper
  /// hyper::Server::bind(&([127, 0, 0, 1], 3030).into())
  ///     .serve(service)
  ///     .await?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn into_service_with_context<T: Send + Sync + 'static>(self, context: T) -> MakeRouterService<T> {
    MakeRouterService(Arc::new(context), self)
  }

  /// Converts the `Treemux` into a `Service` which you can serve directly with `Hyper`.
  /// If you have an existing `Service` that you want to incorporate a `Treemux` into, see
  /// [`Treemux::serve`](crate::Treemux::serve).
  /// ```rust,no_run
  /// # use treemux::{Treemux, RouterBuilder};
  /// # use std::convert::Infallible;
  /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
  /// // Our router...
  /// let router = Treemux::builder();
  ///
  /// // Convert it into a service...
  /// let service = router.into_service();
  ///
  /// // Serve with hyper
  /// hyper::Server::bind(&([127, 0, 0, 1], 3030).into())
  ///     .serve(service)
  ///     .await?;
  /// # Ok(())
  /// # }
  /// ```
  pub fn into_service(self) -> MakeRouterService<()> {
    MakeRouterService(Arc::new(()), self)
  }

  /// An asynchronous function from a `Request` to a `Response`. You will generally not need to use
  /// this function directly, and instead use
  /// [`Treemux::into_service`](crate::Treemux::into_service). However, it may be useful when
  /// incorporating the router into a larger service.
  /// ```rust,no_run
  /// # use treemux::{Treemux, RouterBuilder};
  /// # use hyper::service::{make_service_fn, service_fn};
  /// # use hyper::{Request, Body, Server};
  /// # use std::convert::Infallible;
  /// # use std::sync::Arc;
  ///
  /// # async fn run() {
  /// let mut router = Treemux::builder();
  ///
  /// let router: Arc<Treemux> = Arc::new(router.into());
  ///
  /// let make_svc = make_service_fn(move |_| {
  ///     let router = router.clone();
  ///     async move {
  ///         Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
  ///             let router = router.clone();
  ///             async move { router.serve(req).await }
  ///         }))
  ///     }
  /// });
  ///
  /// let server = Server::bind(&([127, 0, 0, 1], 3000).into())
  ///     .serve(make_svc)
  ///     .await;
  /// # }
  /// ```
  pub async fn serve(&self, req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let method = req.method().clone();
    let mut path = req.uri().path().to_string();

    let has_trailing_slash = path.len() > 1 && path.ends_with('/');
    if has_trailing_slash && self.redirect_trailing_slash {
      path = path.strip_suffix('/').unwrap().to_string();
    }

    let mut match_result = self.root.search(&method, path.clone());

    if match_result.is_none() {
      if self.redirect_clean_path {
        let clean_path: Cow<str> = crate::path::clean(&path).into();
        match_result = self.root.search(&method, clean_path.clone());
        if match_result.is_none() {
          return self.return_response(req, match_result).await;
        }
        if let Some(rdb) = self.redirect_behavior {
          return self.redirect(req, rdb, clean_path).await;
        }
      } else {
        return self.return_response(req, match_result).await;
      }
    }

    let match_result = match_result.unwrap();
    let mut handler = match_result.value.map(|v| v.handler.clone());
    if handler.is_none() {
      let rmeth = req.method();
      if rmeth == Method::OPTIONS && self.handle_global_options.is_some() {
        // req.extensions_mut().insert(match_result.parameters);
        handler = self.handle_global_options.clone();
        let h = handler.unwrap();
        return (h.clone().as_ref())(req).await;
      }

      if handler.is_none() {
        let allowed = match_result.leaf_handler.keys().cloned().collect();
        if let Some(handle_method_not_allowed) = self.handle_method_not_allowed.as_ref() {
          return handle_method_not_allowed(req, allowed).await;
        } else {
          return default_method_not_allowed(allowed).await;
        }
      }
    }

    if (!match_result.is_catch_all || self.remove_catch_all_trailing_slash)
      && has_trailing_slash != match_result.add_slash
      && self.redirect_trailing_slash
    {
      let pth = if match_result.add_slash {
        format!("{}/", path)
      } else {
        path
      };

      if handler.is_some() {
        if let Some(rdb) = self.redirect_behavior {
          return self.redirect(req, rdb, pth.into()).await;
        }
      }
    }

    self.return_response(req, Some(match_result)).await
  }

  async fn redirect(
    &self,
    req: Request<Body>,
    rdb: StatusCode,
    new_path: Cow<'_, str>,
  ) -> Result<Response<Body>, http::Error> {
    Ok(
      Response::builder()
        .status(rdb)
        .header(LOCATION, new_path.as_ref())
        .header(
          CONTENT_TYPE,
          req
            .headers()
            .get(CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| HeaderValue::from_static("text/html; charset=utf-8")),
        )
        .body(Body::empty())
        .unwrap(),
    )
  }

  async fn return_response(
    &self,
    mut req: Request<Body>,
    match_result: Option<Match<'_, Route>>,
  ) -> Result<Response<Body>, http::Error> {
    if let Some(mtc) = match_result {
      req.extensions_mut().insert(mtc.parameters);
      let fut = (mtc.value.unwrap().handler)(req);

      if let Some(handle_panic) = self.handle_panic.as_ref() {
        return (handle_panic)(fut).await;
      } else {
        return handle_panics(fut).await;
      }
    }

    if let Some(handle_not_found) = self.handle_not_found.as_ref() {
      Ok(handle_not_found(req).await.unwrap())
    } else {
      default_not_found().await
    }
  }
}

impl Default for Treemux {
  fn default() -> Self {
    Self {
      root: Node::new(),
      handle_not_found: None,
      handle_method_not_allowed: None,
      handle_global_options: None,
      handle_panic: None,
      head_can_use_get: true,
      redirect_trailing_slash: true,
      redirect_clean_path: true,
      redirect_behavior: Some(StatusCode::MOVED_PERMANENTLY),
      redirect_method_behavior: HashMap::default(),
      remove_catch_all_trailing_slash: false,
      escape_added_routes: false,
    }
  }
}

impl From<Builder> for Treemux {
  fn from(b: Builder) -> Self {
    b.build()
  }
}

pub trait RouterBuilder {
  /// Insert a value into the router for a specific path indexed by a key.
  /// ```rust
  /// use treemux::{Treemux, RouterBuilder};
  /// use hyper::{Response, Body, Method, Request};
  ///
  /// let mut router = Treemux::builder();
  /// router.handle(Method::GET, "/teapot", |_: Request<Body>| {
  ///     async { Ok(Response::new(Body::from("I am a teapot!"))) }
  /// });
  /// let router: Treemux = router.into();
  /// ```
  fn handle<P, H, R>(&self, method: Method, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static;

  /// Register a handler for `GET` requests
  fn get<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::GET, path, handler);
  }

  /// Register a handler for `HEAD` requests
  fn head<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::HEAD, path, handler);
  }

  /// Register a handler for `OPTIONS` requests
  fn options<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::OPTIONS, path, handler);
  }

  /// Register a handler for `POST` requests
  fn post<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::POST, path, handler);
  }

  /// Register a handler for `PUT` requests
  fn put<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::PUT, path, handler);
  }

  /// Register a handler for `PATCH` requests
  fn patch<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::PATCH, path, handler);
  }

  /// Register a handler for `DELETE` requests
  fn delete<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    self.handle(Method::DELETE, path, handler);
  }
}

#[cfg(test)]
mod tests {

  use core::panic;
  use std::{panic::AssertUnwindSafe, sync::Arc};

  use futures::prelude::*;

  use hyper::{
    body,
    header::{HeaderName, HeaderValue},
    http, Body, Method, Request, Response, StatusCode,
  };

  use crate::{Handler, Treemux};

  use super::{Builder, RouterBuilder};

  async fn empty_ok_response(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
    Ok(Response::builder().status(StatusCode::OK).body(Body::empty()).unwrap())
  }

  #[test]
  #[should_panic]
  fn test_empty_router_mapping() {
    Builder::default().get("", empty_ok_response);
  }

  #[tokio::test]
  async fn test_scope_slash_mapping() {
    femme::try_with_level(femme::LevelFilter::Trace).ok();
    let mut b = Builder::default();
    b.scope("/foo").get("/", empty_ok_response);
    let router = b.build();

    let req = Request::builder()
      .method(Method::GET)
      .uri("/foo")
      .body(Body::empty())
      .unwrap();
    let response = router.serve(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);

    let req = Request::builder()
      .method(Method::GET)
      .uri("/foo/")
      .body(Body::empty())
      .unwrap();
    let response = router.serve(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn test_empty_scope_mapping() {
    femme::try_with_level(femme::LevelFilter::Trace).ok();
    let mut b = Builder::default();
    b.scope("/foo").get("", empty_ok_response);
    let router = b.build();

    let req = Request::builder()
      .method(Method::GET)
      .uri("/foo")
      .body(Body::empty())
      .unwrap();
    let response = router.serve(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn test_group_method_scenarios() {
    test_group_methods(false).await;
    test_group_methods(true).await;
  }

  #[tokio::test]
  #[should_panic]
  async fn test_invalid_path() {
    let mut b = Builder::default();
    b.scope("foo");
  }
  #[tokio::test]
  #[should_panic]
  async fn test_invalid_sub_path() {
    let mut b = Builder::default();
    let mut g = b.scope("/foo");
    g.scope("bar");
  }

  #[tokio::test]
  async fn test_set_get_after_head() {
    let make_handler = |method: Method| {
      Box::new(move |_req: Request<Body>| {
        let method = method.clone();
        async move {
          Ok(
            Response::builder()
              .header(
                "x-expected-method",
                HeaderValue::from_str(method.clone().as_str()).unwrap(),
              )
              .body(Body::empty())
              .unwrap(),
          )
        }
      })
    };

    let mut router = Treemux::builder();
    router.head_can_use_get = true;
    router.head("/abc", make_handler(Method::HEAD));
    router.get("/abc", make_handler(Method::GET));

    let mux = Arc::new(router.build());
    let test_method = |method: Method, expect: Method| {
      let router = mux.clone();
      async move {
        let req = Request::builder()
          .uri("/abc")
          .method(method.clone())
          .body(Body::empty())
          .unwrap();
        let result = router.serve(req).await;
        let resp = result.unwrap();
        let result_method = resp
          .headers()
          .get("x-expected-method")
          .and_then(|r| Method::from_bytes(r.as_bytes()).ok());
        if Some(expect.clone()) != result_method {
          panic!(
            "Method {} got result ({}) {:?}, expected {}",
            method,
            resp.status(),
            result_method,
            expect
          );
        }
      }
    };

    test_method(Method::HEAD, Method::HEAD).await;
    test_method(Method::GET, Method::GET).await;
  }

  #[tokio::test]
  async fn test_not_found() {
    let mut router = Treemux::builder();
    router.get("/user/abc", empty_ok_response);

    let mux = router.build();

    let response = mux
      .serve(Request::builder().uri("/abc/").body(Body::empty()).unwrap())
      .await
      .unwrap();

    assert_eq!(StatusCode::NOT_FOUND, response.status());

    let mut router = Treemux::builder();
    router.get("/user/abc", empty_ok_response);
    router.not_found(move |_request| async move {
      Ok(
        Response::builder()
          .status(StatusCode::NOT_FOUND)
          .body(Body::from("custom not found"))
          .unwrap(),
      )
    });
    let mux = router.build();
    let response = mux
      .serve(Request::builder().uri("/abc/").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::NOT_FOUND, response.status());
    assert_eq!(
      "custom not found",
      std::str::from_utf8(body::to_bytes(response.into_body()).await.unwrap().as_ref()).unwrap()
    )
  }

  #[tokio::test]
  async fn test_method_not_allowed_handler() {
    let mut router = Treemux::builder();
    router.get("/user/abc", empty_ok_response);
    router.put("/user/abc", empty_ok_response);
    router.delete("/user/abc", empty_ok_response);
    let mux = router.build();

    let response = mux
      .serve(
        Request::builder()
          .method(Method::POST)
          .uri("/user/abc")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();

    let mut allow: Vec<String> = response
      .headers()
      .get(http::header::ALLOW)
      .unwrap()
      .to_str()
      .unwrap()
      .split(", ")
      .map(|v| v.to_string())
      .collect();
    allow.sort();

    assert_eq!(StatusCode::METHOD_NOT_ALLOWED, response.status());
    assert_eq!("DELETE, GET, HEAD, PUT", allow.join(", "));

    let mut router = Treemux::builder();
    router.get("/user/abc", empty_ok_response);
    router.put("/user/abc", empty_ok_response);
    router.delete("/user/abc", empty_ok_response);
    router.method_not_allowed(move |_request, allowed| async move {
      let mut allowed = allowed.iter().map(|v| v.as_str().to_string()).collect::<Vec<String>>();
      allowed.sort();
      Ok(
        Response::builder()
          .status(StatusCode::METHOD_NOT_ALLOWED)
          .header(http::header::ALLOW, allowed.join(", "))
          .body(Body::from("custom method not allowed"))
          .unwrap(),
      )
    });
    let mux = router.build();

    let response = mux
      .serve(
        Request::builder()
          .method(Method::POST)
          .uri("/user/abc")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();

    let mut allow: Vec<String> = response
      .headers()
      .get(http::header::ALLOW)
      .unwrap()
      .to_str()
      .unwrap()
      .split(", ")
      .map(|v| v.to_string())
      .collect();
    allow.sort();

    assert_eq!(StatusCode::METHOD_NOT_ALLOWED, response.status());
    assert_eq!("DELETE, GET, HEAD, PUT", allow.join(", "));
    assert_eq!(
      "custom method not allowed",
      std::str::from_utf8(body::to_bytes(response.into_body()).await.unwrap().as_ref()).unwrap()
    )
  }

  #[tokio::test]
  async fn test_handle_options() {
    let make_router = || {
      let mut router = Treemux::builder();
      router.get("/user/abc", empty_ok_response);
      router.put("/user/abc", empty_ok_response);
      router.delete("/user/abc", empty_ok_response);
      router.options("/user/abc/options", |_req| async {
        Ok(
          Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "treemux.rs")
            .body(Body::empty())
            .unwrap(),
        )
      });
      router
    };

    let mux = make_router().build();
    let res = mux
      .serve(
        Request::builder()
          .method(Method::OPTIONS)
          .uri("/user/abc")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(StatusCode::METHOD_NOT_ALLOWED, res.status());

    let mut builder = make_router();
    builder.global_options(|_req| async {
      Ok(
        Response::builder()
          .status(StatusCode::NO_CONTENT)
          .header(http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
          .body(Body::empty())
          .unwrap(),
      )
    });
    let mux = builder.build();
    let res = mux
      .serve(
        Request::builder()
          .method(Method::OPTIONS)
          .uri("/user/abc")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(StatusCode::NO_CONTENT, res.status());
    assert_eq!(
      "*",
      res
        .headers()
        .get(http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .unwrap()
        .to_str()
        .unwrap()
    );

    let res = mux
      .serve(
        Request::builder()
          .method(Method::OPTIONS)
          .uri("/user/abc/options")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
    assert_eq!(
      "treemux.rs",
      res
        .headers()
        .get(http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .unwrap()
        .to_str()
        .unwrap()
    );

    let builder = make_router();
    let mux = builder.build();
    let res = mux
      .serve(
        Request::builder()
          .method(Method::OPTIONS)
          .uri("/user/abc/options")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(StatusCode::UNAUTHORIZED, res.status());
    assert_eq!(
      "treemux.rs",
      res
        .headers()
        .get(http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .unwrap()
        .to_str()
        .unwrap()
    );

    let mut builder = make_router();
    builder.global_options(|_req| async {
      Ok(
        Response::builder()
          .status(StatusCode::NO_CONTENT)
          .header(http::header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
          .body(Body::empty())
          .unwrap(),
      )
    });
    let mux = builder.build();
    let res = mux
      .serve(
        Request::builder()
          .method(Method::POST)
          .uri("/user/abc")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(StatusCode::METHOD_NOT_ALLOWED, res.status());
    let mut allow: Vec<String> = res
      .headers()
      .get(http::header::ALLOW)
      .unwrap()
      .to_str()
      .unwrap()
      .split(", ")
      .map(|v| v.to_string())
      .collect();
    allow.sort();

    assert_eq!("DELETE, GET, HEAD, PUT", allow.join(", "));
  }

  #[tokio::test]
  async fn test_panic() {
    let mut router = Treemux::builder();
    router.get("/abc", |_req| async {
      panic!("expected");
    });
    let mux = router.build();
    let res = mux
      .serve(Request::builder().uri("/abc").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, res.status());

    let mut router = Treemux::builder();
    router.get("/abc", |_req| async {
      panic!("expected");
    });
    router.handle_panics(|fut| async {
      let wrapped = AssertUnwindSafe(fut).catch_unwind();
      match wrapped.await {
        Ok(response) => response,
        Err(panic) => {
          let error = Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("X-Result", HeaderValue::from_static("done"))
            .body(Body::from(format!("{:?}", panic)))
            .unwrap();
          Ok(error)
        }
      }
    });
    let mux = router.build();
    let res = mux
      .serve(Request::builder().uri("/abc").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::INTERNAL_SERVER_ERROR, res.status());
    assert_eq!("done", res.headers().get("X-Result").unwrap().to_str().unwrap());
  }

  #[tokio::test]
  async fn test_builder() {
    femme::try_with_level(femme::LevelFilter::Trace).ok();
    let mut b = Builder::default();
    b.middleware(log_request);
    b.get("/hello", hello_world);
    b.get("/other", hello_again_world);

    let mut grp = Builder::default();
    grp.get("/hello", hello_world);
    grp.post("/hello", hello_world);
    grp.get("/other", hello_again_world);
    b.extend("/api/:user/:thing", grp);

    let mut scp = b.scope("/apps/:appid/user");
    scp.middleware(debug_log_request);
    scp.get("/hello", nested_world);
    scp.post("/hello", nested_world);

    let tm: Treemux = b.into();

    let resp = tm
      .serve(Request::builder().uri("/hello").body(Body::empty()).unwrap())
      .await
      .unwrap();
    info!("/hello: {:?}", resp);
    let resp = tm
      .serve(Request::builder().uri("/other").body(Body::empty()).unwrap())
      .await
      .unwrap();
    info!("/other: {:?}", resp);

    let resp = tm
      .serve(
        Request::builder()
          .uri("/apps/kohana/user/hello")
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    info!("/apps/kohana/user/hello: {:?}", resp);
  }

  async fn hello_world(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
    info!("inside the handler");
    let resp = Response::builder().body(Body::from("Hello, World!"));
    info!("created response, returning from handler");
    resp
  }

  async fn hello_again_world(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
    Response::builder().body(Body::from("Hello again, World!"))
  }

  async fn nested_world(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
    Response::builder().body(Body::from("Nested World!"))
  }

  fn log_request<H, R>(f: H) -> Handler
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    Box::new(move |req: Request<Body>| {
      info!("before calling f");
      let fut = f(req);
      Box::pin(async move {
        info!("before await");
        let resp = fut.await;
        info!("after response");
        resp
      })
    })
  }

  fn debug_log_request<H, R>(f: H) -> Handler
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
  {
    Box::new(move |req: Request<Body>| {
      debug!("before calling f");
      let fut = f(req);
      Box::pin(async move {
        debug!("before await");
        let resp = fut.await;
        debug!("after response");
        resp
      })
    })
  }

  async fn test_group_methods(head_can_use_get: bool) {
    let make_handler = |method: Method| {
      Box::new(move |_req: Request<Body>| {
        let method = method.clone();
        async move {
          Ok(
            Response::builder()
              .header(
                "x-expected-method",
                HeaderValue::from_str(method.clone().as_str()).unwrap(),
              )
              .body(Body::empty())
              .unwrap(),
          )
        }
      })
    };

    let make_router = || {
      let mut router = Treemux::builder();
      router.head_can_use_get = head_can_use_get;

      let mut b = router.scope("/base");
      let mut g = b.scope("/user");
      g.get("/:param", make_handler(Method::GET));
      g.post("/:param", make_handler(Method::POST));
      g.patch("/:param", make_handler(Method::PATCH));
      g.put("/:param", make_handler(Method::PUT));
      g.delete("/:param", make_handler(Method::DELETE));
      router
    };

    let test_method = |router: Arc<Treemux>, method: Method, expect: Option<Method>| async move {
      let router = router.clone();
      let req = Request::builder()
        .uri(format!("/base/user/{}", method.as_str()))
        .method(method.clone())
        .body(Body::empty())
        .unwrap();
      let result = router.serve(req).await;
      let resp = result.unwrap();
      match expect {
        Some(expect) => {
          let result_method = resp
            .headers()
            .get("x-expected-method")
            .and_then(|r| Method::from_bytes(r.as_bytes()).ok());
          if Some(expect.clone()) != result_method {
            panic!(
              "Method {} got result ({}) {:?}, expected {}",
              method,
              resp.status(),
              result_method,
              expect
            );
          }
        }
        None => {
          if resp.status() != StatusCode::METHOD_NOT_ALLOWED {
            panic!("Method {} not expected to match but saw code {}", method, resp.status());
          }
        }
      }
    };

    let router = Arc::new(make_router().build());

    test_method(router.clone(), Method::GET, Some(Method::GET)).await;
    test_method(router.clone(), Method::POST, Some(Method::POST)).await;
    test_method(router.clone(), Method::PATCH, Some(Method::PATCH)).await;
    test_method(router.clone(), Method::PUT, Some(Method::PUT)).await;
    test_method(router.clone(), Method::DELETE, Some(Method::DELETE)).await;
    if head_can_use_get {
      info!("test implicit head with head_can_use_get = true");
      test_method(router.clone(), Method::HEAD, Some(Method::GET)).await;
    } else {
      info!("test implicit head with head_can_use_get = false");
      test_method(router.clone(), Method::HEAD, None).await;
    }

    let mut router = make_router();
    router.head("/base/user/:param", make_handler(Method::HEAD));
    test_method(Arc::new(router.build()), Method::HEAD, Some(Method::HEAD)).await;
  }
}
