//! [`Treemux`](crate::Treemux) is a lightweight high performance HTTP request router.
//!
//! This router supports variables in the routing pattern and matches against
//! the request method. It also scales better.
//!
//! The router is optimized for high performance and a small memory footprint.
//! It scales well even with very long paths and a large number of routes.
//! A compressing dynamic trie (radix tree) structure is used for efficient matching.
//!
//! With the `hyper-server` feature enabled, the `Router` can be used as a router for a hyper server:
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
//! ```ignore
//!  # use treemux::tree::Params;
//!  # let params = Params::default();

//!  let user = params.by_name("user") // defined by :user or *user
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
  tree::{Error, HandlerConfig, Node, Params},
  RedirectBehavior,
};
use hyper::{header, http, Body, Method, Request, Response, StatusCode};
use std::sync::{Arc, Mutex};
use std::{borrow::Cow, collections::HashMap};
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
  pub fn extend<B: Into<Treemux>>(&self, routes: B) {
    self
      .inner
      .root
      .lock()
      .unwrap()
      .extend(self.prefix.to_string().into(), routes.into().root)
  }

  pub fn scope<'b, P: Into<Cow<'static, str>>>(&'b mut self, path: P) -> GroupBuilder<'b> {
    GroupBuilder {
      prefix: format!("{}{}", self.prefix, path.into()).into(),
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
    let mut root = self.inner.root.lock().unwrap();
    let newp: Cow<str> = format!("{}{}", self.prefix, path.into()).into();
    let req_handler = self.chain.clone().chain(Box::new(move |req| Box::pin(handler(req))));
    root.insert(HandlerConfig::new(method, newp.clone(), Route::new(newp, req_handler)));
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

pub struct Builder {
  path: Cow<'static, str>,
  root: Arc<Mutex<Node<'static, Route>>>,
  chain: Arc<dyn Middleware<Input = Handler, Output = Handler>>,
  handle_not_found: Option<Handler>,
  handle_method_not_allowed: Option<MethodNotAllowedHandler>,
  handle_global_options: Option<Handler>,
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
  /// Sets the default redirect behavior when RedirectTrailingSlash or
  /// RedirectCleanPath are true. The default value is `Redirect301`.
  pub redirect_behavior: RedirectBehavior,

  /// Overrides the default behavior for a particular HTTP method.
  /// The key is the method name, and the value is the behavior to use for that method.
  pub redirect_method_behavior: HashMap<Method, RedirectBehavior>,

  /// Removes the trailing slash when a catch-all pattern
  /// is matched, if set to true. By default, catch-all paths are never redirected.
  pub remove_catach_all_trailing_slash: bool,
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
      head_can_use_get: true,
      redirect_trailing_slash: true,
      redirect_clean_path: true,
      redirect_behavior: RedirectBehavior::Redirect301,
      redirect_method_behavior: HashMap::default(),
      remove_catach_all_trailing_slash: false,
    }
  }
}

impl Builder {
  pub fn extend<P: Into<Cow<'static, str>>, B: Into<Treemux>>(&self, path: P, routes: B) {
    self.root.lock().unwrap().extend(path.into(), routes.into().root)
  }

  pub fn scope<'b, P: Into<Cow<'b, str>>>(&'b mut self, path: P) -> GroupBuilder<'b> {
    GroupBuilder {
      prefix: format!("{}{}", self.path, path.into()).into(),
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

  fn build(self) -> Treemux {
    let root = Arc::try_unwrap(self.root)
      .map_err(|_| ())
      .unwrap()
      .into_inner()
      .unwrap();
    let mut result = Treemux::new(root);
    result.handle_not_found = self.handle_not_found;
    result.handle_method_not_allowed = self.handle_method_not_allowed;
    result.handle_global_options = self.handle_global_options;
    result.head_can_use_get = self.head_can_use_get;
    result.redirect_trailing_slash = self.redirect_trailing_slash;
    result.redirect_clean_path = self.redirect_clean_path;
    result.redirect_behavior = self.redirect_behavior;
    result.redirect_method_behavior = self.redirect_method_behavior;
    result.remove_catach_all_trailing_slash = self.remove_catach_all_trailing_slash;
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
    let mut root = self.root.lock().unwrap();

    let path: Cow<str> = path.into().into();
    check_path(path.clone());
    let newp: Cow<str> = format!("{}{}", self.path, path).into();
    if newp.is_empty() {
      panic!("Cannot map an empty path");
    }

    let (add_slash, newp) = if newp.len() > 1 && newp.ends_with('/') && self.redirect_trailing_slash {
      (true, newp.strip_suffix('/').unwrap_or_default().to_string().into())
    } else {
      (false, newp)
    };
    let req_handler = self.chain.clone().chain(Box::new(move |req| Box::pin(handler(req))));
    let mut hcfg = HandlerConfig::new(method, newp.clone(), Route::new(newp, req_handler));
    hcfg.add_slash = add_slash;
    root.insert(hcfg)
  }
}

pub struct Treemux {
  root: Node<'static, Route>,
  handle_not_found: Option<Handler>,
  handle_method_not_allowed: Option<MethodNotAllowedHandler>,
  handle_global_options: Option<Handler>,
  head_can_use_get: bool,
  redirect_trailing_slash: bool,
  redirect_clean_path: bool,
  redirect_behavior: RedirectBehavior,
  redirect_method_behavior: HashMap<Method, RedirectBehavior>,
  remove_catach_all_trailing_slash: bool,
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
    self.root.search(method, path).map_err(|_| false).map(|m| {
      let vv = m.value.unwrap();
      (vv.handler.clone(), m.parameters.clone())
    })
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
  pub async fn serve(&self, mut req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    match self.root.search(&method, path) {
      Ok(mtc) => {
        req.extensions_mut().insert(mtc.parameters);
        (mtc.value.unwrap().handler)(req).await
      }
      Err(Error::NotFound(_)) => {
        if let Some(handle_not_found) = self.handle_not_found.as_ref() {
          Ok(handle_not_found(req).await.unwrap())
        } else {
          default_not_found().await
        }
      }
      Err(Error::MethodNotAllowed(_, allowed)) => {
        if let Some(handle_method_not_allowed) = self.handle_method_not_allowed.as_ref() {
          handle_method_not_allowed(req, allowed.clone()).await
        } else {
          default_method_not_allowed(allowed).await
        }
      }
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
      head_can_use_get: true,
      redirect_trailing_slash: true,
      redirect_clean_path: true,
      redirect_behavior: RedirectBehavior::Redirect301,
      redirect_method_behavior: HashMap::default(),
      remove_catach_all_trailing_slash: false,
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

  use futures::prelude::*;
  use hyper::{http, Body, Request, Response};

  use crate::{Handler, Treemux};

  use super::{Builder, RouterBuilder};

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
}
