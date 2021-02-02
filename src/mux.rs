//! [`Treemux`](crate::Treemux) is a lightweight high performance HTTP request router.
//!
//! This router supports variables in the routing pattern and matches against
//! the request method. It also scales better.
//!
//! The router is optimized for high performance and a small memory footprint.
//! It scales well even with very long paths and a large number of routes.
//! A compressing dynamic trie (radix tree) structure is used for efficient matching.
//!
//! This project is built around the base foundation provided by tower-layer and tower-service.
//! So if you have existing tower layers (middlewares), you should be able to reuse them.
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

use std::{any::Any, borrow::Cow, collections::HashMap, net::SocketAddr, panic::AssertUnwindSafe, pin::Pin, sync::Arc};

use futures::{Future, FutureExt};

use hyper::{
  header::{HeaderValue, ALLOW, CONTENT_TYPE, LOCATION},
  http, Body, Method, Request, Response, StatusCode,
};
use std::sync::Mutex;
use tower_layer::{layer_fn, Identity, Layer, Stack};
use tower_service::Service;

use crate::{
  serve::MakeRouterService,
  tree::{HandlerConfig, LookupResult, Node},
  Params, RedirectBehavior,
};

pub trait RequestExt {
  fn params(&self) -> &Params;
  fn remote_addr(&self) -> SocketAddr;
  fn app_context<T: Send + Sync + 'static>(&self) -> Option<Arc<T>>;
}

impl RequestExt for Request<Body> {
  fn params(&self) -> &Params {
    self.extensions().get::<Params>().unwrap()
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
  fn handle<P, H>(&self, method: Method, path: P, handler: H)
  where
    P: Into<String>,
    H: Into<RequestHandler>;

  /// Register a handler for `GET` requests
  fn get<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::GET, path, handler);
  }

  /// Register a handler for `HEAD` requests
  fn head<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::HEAD, path, handler);
  }

  /// Register a handler for `OPTIONS` requests
  fn options<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::OPTIONS, path, handler);
  }

  /// Register a handler for `POST` requests
  fn post<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::POST, path, handler);
  }

  /// Register a handler for `PUT` requests
  fn put<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::PUT, path, handler);
  }

  /// Register a handler for `PATCH` requests
  fn patch<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::PATCH, path, handler);
  }

  /// Register a handler for `DELETE` requests
  fn delete<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = ResponseResult> + Send + 'static,
  {
    self.handle(Method::DELETE, path, handler);
  }
}

pub type HttpResult<T> = Result<T, http::Error>;
pub type ResponseResult = HttpResult<Response<Body>>;
pub type ResponseFut = Pin<Box<dyn Future<Output = ResponseResult> + Send + 'static>>;
pub type Handler = Box<dyn Fn(Request<Body>) -> ResponseFut + Send + Sync>;
type HandlerArc = Arc<Handler>;
pub type PanicHandler = Arc<dyn Fn(Box<dyn Any + Send>) -> ResponseFut + Send + Sync + 'static>;

#[derive(Clone)]
pub struct AllowedMethods(Vec<Method>);
impl AllowedMethods {
  pub fn methods(&self) -> &[Method] {
    &self.0
  }
}

#[derive(Clone)]
pub struct RequestHandler {
  cb: HandlerArc,
}

impl<F, R> From<F> for RequestHandler
where
  F: Fn(Request<Body>) -> R + Send + Sync + 'static,
  R: Future<Output = ResponseResult> + Send + 'static,
{
  fn from(f: F) -> Self {
    RequestHandler {
      cb: Arc::new(Box::new(move |req| Box::pin(f(req)))),
    }
  }
}

impl Service<Request<Body>> for RequestHandler {
  type Response = Response<Body>;

  type Error = http::Error;

  type Future = ResponseFut;

  fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
    std::task::Poll::Ready(Ok(()))
  }

  fn call(&mut self, req: Request<Body>) -> Self::Future {
    (self.cb.clone())(req)
  }
}

pub struct GroupBuilder<'a, M, S> {
  prefix: Cow<'a, str>,
  inner: &'a Builder<S>,
  stack: M,
}

impl<'a, M, S> GroupBuilder<'a, M, S> {
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

  pub fn scope<P: Into<Cow<'static, str>>>(self, path: P) -> GroupBuilder<'a, M, S> {
    let p: Cow<str> = path.into();
    if p.is_empty() {
      panic!("Scope path must not be empty");
    }

    check_path(p.clone());
    let pth = format!("{}{}", self.prefix, p.as_ref());

    GroupBuilder {
      prefix: (&pth).strip_suffix('/').unwrap_or(&pth).to_string().into(),
      inner: self.inner,
      stack: self.stack,
    }
  }

  pub fn middleware<N>(self, middleware: N) -> GroupBuilder<'a, Stack<N, M>, S>
  where
    N: Layer<S::Service, Service = S::Service>,
    M: Layer<S::Service, Service = S::Service>,
    S: Layer<RequestHandler, Service = RequestHandler>,
  {
    GroupBuilder {
      prefix: self.prefix,
      inner: self.inner,
      stack: Stack::new(middleware, self.stack),
    }
  }
}

impl<'a, M, S> RouterBuilder for GroupBuilder<'a, M, S>
where
  S: Layer<RequestHandler, Service = RequestHandler>,
  M: Layer<RequestHandler, Service = RequestHandler>,
{
  fn handle<P, H>(&self, method: Method, path: P, handler: H)
  where
    P: Into<String>,
    H: Into<RequestHandler>,
  {
    let path: Cow<str> = path.into().into();
    let path: Cow<str> = format!("{}{}", self.prefix, path).into();
    check_path(path.clone());

    self.inner.add_route(method, path, self.stack.layer(handler.into()))
  }
}

pub struct Builder<M> {
  path: Cow<'static, str>,
  root: Arc<Mutex<Node<'static, RequestHandler>>>,
  stack: M,
  handle_not_found: Option<RequestHandler>,
  handle_method_not_allowed: Option<RequestHandler>,
  handle_global_options: Option<RequestHandler>,
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
  pub redirect_behavior: Option<RedirectBehavior>,

  /// Overrides the default behavior for a particular HTTP method.
  /// The key is the method name, and the value is the behavior to use for that method.
  pub redirect_method_behavior: HashMap<Method, RedirectBehavior>,

  /// Removes the trailing slash when a catch-all pattern
  /// is matched, if set to true. By default, catch-all paths are never redirected.
  pub remove_catch_all_trailing_slash: bool,

  /// Controls URI escaping behavior when adding a route to the tree.
  /// If set to true, the router will add both the route as originally passed, and
  /// a version passed through url::parse. This behavior is disabled by default.
  pub escape_added_routes: bool,
}

impl Default for Builder<Identity> {
  fn default() -> Self {
    Self {
      path: "".into(),
      root: Arc::new(Mutex::new(Node::new())),
      stack: Identity::new(),
      handle_not_found: None,
      handle_method_not_allowed: None,
      handle_global_options: None,
      handle_panic: None,
      head_can_use_get: true,
      redirect_trailing_slash: true,
      redirect_clean_path: true,
      redirect_behavior: Some(RedirectBehavior::Redirect301),
      redirect_method_behavior: HashMap::default(),
      remove_catch_all_trailing_slash: false,
      escape_added_routes: false,
    }
  }
}

impl<M> Builder<M> {
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

  pub fn scope<'b, P: Into<Cow<'b, str>>>(&'b mut self, path: P) -> GroupBuilder<'b, Identity, M>
  where
    M: Layer<RequestHandler, Service = RequestHandler>,
  {
    let p: Cow<str> = path.into();
    if p.is_empty() {
      panic!("Scope path must not be empty");
    }

    check_path(p.clone());
    let pth = format!("{}{}", self.path, p.as_ref());
    GroupBuilder {
      prefix: (&pth).strip_suffix('/').unwrap_or(&pth).to_string().into(),
      inner: self,
      stack: Identity::new(),
    }
  }

  pub fn not_found<H: Into<RequestHandler>>(&mut self, handler: H)
  where
    M: Layer<RequestHandler, Service = RequestHandler>,
  {
    let rh: RequestHandler = handler.into();
    let hn = self.stack.layer(rh);
    self.handle_not_found = Some(hn);
  }

  /// Register a handler for when the path matches a different method than the requested one
  pub fn method_not_allowed<H: Into<RequestHandler>>(&mut self, handler: H)
  where
    M: Layer<RequestHandler, Service = RequestHandler>,
  {
    let rh: RequestHandler = handler.into();
    let hn = self.stack.layer(rh);
    self.handle_method_not_allowed = Some(hn);
  }

  /// Register a handler for when the path matches a different method than the requested one
  pub fn global_options<H: Into<RequestHandler>>(&mut self, handler: H)
  where
    M: Layer<RequestHandler, Service = RequestHandler>,
  {
    let req_handler = handler.into();
    self.handle_global_options = Some(self.stack.layer(req_handler));
  }

  pub fn handle_panics<H, R>(&mut self, handler: H)
  where
    R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
    H: Fn(Box<dyn Any + Send>) -> R + Send + Sync + 'static,
  {
    let ph: PanicHandler = Arc::new(move |fut| Box::pin(handler(fut)));
    self.handle_panic = Some(ph);
  }

  pub fn middleware<N>(self, middleware: N) -> Builder<Stack<N, M>>
  where
    N: Layer<M::Service, Service = M::Service>,
    M: Layer<RequestHandler, Service = RequestHandler>,
  {
    Builder {
      handle_not_found: self.handle_not_found,
      handle_method_not_allowed: self.handle_method_not_allowed,
      stack: Stack::new(middleware, self.stack),
      root: self.root,
      redirect_trailing_slash: self.redirect_trailing_slash,
      path: self.path,
      handle_global_options: self.handle_global_options,
      handle_panic: self.handle_panic,
      head_can_use_get: self.head_can_use_get,
      redirect_clean_path: self.redirect_clean_path,
      redirect_behavior: self.redirect_behavior,
      redirect_method_behavior: self.redirect_method_behavior,
      remove_catch_all_trailing_slash: self.remove_catch_all_trailing_slash,
      escape_added_routes: self.escape_added_routes,
    }
  }

  fn add_route<P: Into<String>, F>(&self, method: Method, pattern: P, handler: F)
  where
    M: Layer<RequestHandler, Service = RequestHandler>,
    F: Into<RequestHandler>,
  {
    let pstr = pattern.into();
    let patn: Cow<str> = pstr.into();

    if patn.is_empty() {
      panic!("Cannot map an empty path");
    }

    let mut root = self.root.lock().unwrap();
    let (add_slash, patn) = if patn.len() > 1 && patn.ends_with('/') && self.redirect_trailing_slash {
      (true, patn.strip_suffix('/').unwrap_or_default().to_string().into())
    } else {
      (false, patn)
    };

    let rh: RequestHandler = handler.into();
    let mut config = HandlerConfig::new(method.clone(), patn.clone(), self.stack.layer(rh.clone()));
    config.add_slash = add_slash;
    config.head_can_use_get = self.head_can_use_get;
    root.insert(config);

    if self.escape_added_routes {
      let u: http::Uri = patn.clone().parse().unwrap();
      let escaped_path: Cow<str> = u.path().to_owned().into();
      if escaped_path != patn {
        let mut hcfg = HandlerConfig::new(method, escaped_path.clone(), self.stack.layer(rh));
        hcfg.add_slash = add_slash;
        hcfg.head_can_use_get = self.head_can_use_get;
        root.insert(hcfg);
      }
    }
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

  fn build(self) -> Treemux {
    let root = Arc::try_unwrap(self.root)
      .map_err(|_| ())
      .unwrap()
      .into_inner()
      .unwrap();

    Treemux {
      root,
      handle_not_found: self.handle_not_found,
      handle_method_not_allowed: self.handle_method_not_allowed,
      handle_global_options: self.handle_global_options,
      handle_panic: self.handle_panic,
      head_can_use_get: self.head_can_use_get,
      redirect_trailing_slash: self.redirect_trailing_slash,
      redirect_clean_path: self.redirect_clean_path,
      redirect_behavior: self.redirect_behavior,
      redirect_method_behavior: self.redirect_method_behavior,
      remove_catch_all_trailing_slash: self.remove_catch_all_trailing_slash,
      escape_added_routes: self.escape_added_routes,
    }
  }
}

fn check_path(path: Cow<str>) {
  if !path.starts_with('/') {
    panic!("Path {} must start with a slash", path);
  }
}

impl<M> RouterBuilder for Builder<M>
where
  M: Layer<RequestHandler, Service = RequestHandler>,
{
  fn handle<P, H>(&self, method: Method, path: P, handler: H)
  where
    P: Into<String>,
    H: Into<RequestHandler>,
  {
    let path: Cow<str> = path.into().into();
    check_path(path.clone());
    let path: Cow<str> = format!("{}{}", self.path, path).into();
    self.add_route(method, path, handler)
  }
}

pub struct Treemux {
  handle_not_found: Option<RequestHandler>,
  handle_method_not_allowed: Option<RequestHandler>,
  root: Node<'static, RequestHandler>,
  redirect_trailing_slash: bool,
  handle_global_options: Option<RequestHandler>,
  handle_panic: Option<PanicHandler>,
  head_can_use_get: bool,
  redirect_clean_path: bool,
  redirect_behavior: Option<RedirectBehavior>,
  redirect_method_behavior: HashMap<Method, RedirectBehavior>,
  remove_catch_all_trailing_slash: bool,
  escape_added_routes: bool,
}

pub fn middleware_fn<F, R, Q>(middleware: F) -> impl Layer<RequestHandler, Service = RequestHandler>
where
  R: Fn(Request<Body>) -> Q,
  Q: Future<Output = ResponseResult> + Send + 'static,
  F: Fn(Handler) -> R + Send + Sync + 'static,
{
  let mw = Arc::new(middleware);
  layer_fn(move |svc: RequestHandler| {
    let middleware = mw.clone();
    let handle = svc.cb.clone();
    RequestHandler {
      cb: Arc::new(Box::new(move |req| {
        let handle = handle.clone();
        let rh = Box::new(move |ir| handle(ir));
        Box::pin(middleware(rh)(req))
      })),
    }
  })
}

impl std::fmt::Debug for Treemux {
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

impl std::fmt::Display for Treemux {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&self.root.dump_tree("", ""))
  }
}

impl Treemux {
  pub fn builder() -> Builder<Identity> {
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
  pub fn lookup<'b, P: AsRef<str>>(&'b self, method: &'b Method, path: P) -> Result<(RequestHandler, Params), bool> {
    self
      .root
      .search(method, path)
      .map(|m| {
        let vv = m.value.unwrap();
        Ok((vv.clone(), m.parameters.clone()))
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
  /// # use std::sync::{Arc, Mutex};
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
  pub async fn serve(&self, mut req: Request<Body>) -> ResponseResult {
    let method = req.method().clone();
    let mut path = req.uri().path().to_string();

    let has_trailing_slash = path.len() > 1 && path.ends_with('/');
    if has_trailing_slash && self.redirect_trailing_slash {
      path = path.strip_suffix('/').unwrap().to_string();
    }

    let mut match_result = self.root.lookup(method.clone(), path.clone());
    let redirect_behavior = self.redirect_method_behavior.get(&method).copied();
    let redirect_behavior = redirect_behavior.or(self.redirect_behavior);
    let redirect_behavior = redirect_behavior.filter(|v| *v != RedirectBehavior::UseHandler);

    if match_result.is_none() {
      if self.redirect_clean_path {
        let clean_path: Cow<str> = crate::path::clean(&path).into();
        match_result = self.root.lookup(method, clean_path.clone());
        if match_result.is_none() {
          return self.return_response(req, match_result).await;
        }
        if let Some(rdb) = redirect_behavior {
          return self.redirect(req, rdb, clean_path).await;
        }
      } else {
        return self.return_response(req, match_result).await;
      }
    }

    let match_result = match_result.unwrap();
    let mut handler = match_result.value.clone();
    if handler.is_none() {
      let rmeth = req.method();
      if rmeth == Method::OPTIONS && self.handle_global_options.is_some() {
        // req.extensions_mut().insert(match_result.parameters);
        handler = self.handle_global_options.clone();
        let h = handler.as_mut().unwrap();
        return h.call(req).await;
      }

      if handler.is_none() {
        let allowed = match_result.leaf_handler.keys().cloned().collect();
        if let Some(mut handle_method_not_allowed) = self.handle_method_not_allowed.clone() {
          req.extensions_mut().insert(AllowedMethods(allowed));
          return handle_method_not_allowed.call(req).await;
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
        if let Some(rdb) = redirect_behavior {
          return self.redirect(req, rdb, pth.into()).await;
        }
      }
    }

    self.return_response(req, Some(match_result)).await
  }

  fn redirect(&self, req: Request<Body>, rdb: RedirectBehavior, new_path: Cow<'_, str>) -> ResponseFut {
    let new_uri = if let Some(qs) = req.uri().query() {
      format!("{}?{}", new_path, qs)
    } else {
      new_path.to_string()
    };
    let sc = match rdb {
      RedirectBehavior::Redirect307 => StatusCode::TEMPORARY_REDIRECT,
      RedirectBehavior::Redirect308 => StatusCode::PERMANENT_REDIRECT,
      _ => StatusCode::MOVED_PERMANENTLY,
    };
    Box::pin(async move {
      Ok(
        Response::builder()
          .status(sc)
          .header(LOCATION, new_uri)
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
    })
  }

  async fn return_response(
    &self,
    mut req: Request<Body>,
    match_result: Option<LookupResult<RequestHandler>>,
  ) -> ResponseResult {
    if let Some(mtc) = match_result.clone() {
      req.extensions_mut().insert(mtc.parameters.clone());
      let fut = mtc.value.unwrap().call(req);

      if let Some(handle_panic) = self.handle_panic.clone() {
        let fut = AssertUnwindSafe(fut).catch_unwind();
        match fut.await {
          Ok(response) => return response,
          Err(panic) => {
            return handle_panic(panic).await;
          }
        }
      } else {
        return handle_panics(fut).await;
      }
    }

    if let Some(mut handle_not_found) = self.handle_not_found.clone() {
      handle_not_found.call(req).await
    } else {
      default_not_found().await
    }
  }
}

async fn default_not_found() -> ResponseResult {
  Response::builder()
    .status(StatusCode::NOT_FOUND)
    .body(Body::from(format!("{}", StatusCode::NOT_FOUND)))
    .map_err(Into::into)
}

async fn default_method_not_allowed(allow: Vec<Method>) -> ResponseResult {
  Response::builder()
    .status(StatusCode::METHOD_NOT_ALLOWED)
    .header(
      ALLOW,
      allow
        .iter()
        .map(|v| v.as_str().to_string())
        .collect::<Vec<String>>()
        .join(", "),
    )
    .body(Body::from(format!("{}", StatusCode::METHOD_NOT_ALLOWED)))
    .map_err(Into::into)
}

async fn handle_panics(fut: impl Future<Output = ResponseResult>) -> ResponseResult {
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

impl<M> From<Builder<M>> for Treemux {
  fn from(b: Builder<M>) -> Self {
    b.build()
  }
}

// impl Service<Request<Body>> for Treemux {
//   type Response = Response<Body>;

//   type Error = http::Error;

//   type Future = Pin<Box<dyn Future<Output = ResponseResult> + Send>>;

//   fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
//     std::task::Poll::Ready(Ok(()))
//   }

//   fn call(&mut self, mut req: Request<Body>) -> Self::Future {
//     let method = req.method().clone();
//     let mut path = req.uri().path().to_string();

//     let has_trailing_slash = path.len() > 1 && path.ends_with('/');
//     if has_trailing_slash && self.redirect_trailing_slash {
//       path = path.strip_suffix('/').unwrap().to_string();
//     }

//     let mut match_result = self.root.lookup(method.clone(), path.clone());
//     let redirect_behavior = self.redirect_method_behavior.get(&method).copied();
//     let redirect_behavior = redirect_behavior.or(self.redirect_behavior);
//     let redirect_behavior = redirect_behavior.filter(|v| *v != RedirectBehavior::UseHandler);

//     if match_result.is_none() {
//       if self.redirect_clean_path {
//         let clean_path: Cow<str> = crate::path::clean(&path).into();
//         match_result = self.root.lookup(method, clean_path.clone());
//         if match_result.is_none() {
//           if let Some(mtc) = match_result {
//             req.extensions_mut().insert(mtc.parameters.clone());
//             let fut = mtc.value.unwrap().call(req);

//             if let Some(handle_panic) = self.handle_panic.clone() {
//               let fut = AssertUnwindSafe(fut).catch_unwind();
//               return Box::pin(async move {
//                 match fut.await {
//                   Ok(response) => response,
//                   Err(panic) => handle_panic(panic).await,
//                 }
//               });
//             } else {
//               return Box::pin(handle_panics(fut));
//             }
//           }

//           if let Some(mut handle_not_found) = self.handle_not_found.clone() {
//             return handle_not_found.call(req);
//           } else {
//             return Box::pin(default_not_found());
//           }
//         }
//         if let Some(rdb) = redirect_behavior {
//           return self.redirect(req, rdb, clean_path);
//         }
//       } else {
//         if let Some(mtc) = match_result {
//           req.extensions_mut().insert(mtc.parameters.clone());
//           let fut = mtc.value.unwrap().call(req);

//           if let Some(handle_panic) = self.handle_panic.clone() {
//             let fut = AssertUnwindSafe(fut).catch_unwind();
//             return Box::pin(async move {
//               match fut.await {
//                 Ok(response) => response,
//                 Err(panic) => handle_panic(panic).await,
//               }
//             });
//           } else {
//             return Box::pin(handle_panics(fut));
//           }
//         }

//         if let Some(mut handle_not_found) = self.handle_not_found.clone() {
//           return handle_not_found.call(req);
//         } else {
//           return Box::pin(default_not_found());
//         }
//       }
//     }

//     let match_result = match_result.unwrap();
//     let mut handler = match_result.value.clone();
//     if handler.is_none() {
//       let rmeth = req.method();
//       if rmeth == Method::OPTIONS && self.handle_global_options.is_some() {
//         // req.extensions_mut().insert(match_result.parameters);
//         handler = self.handle_global_options.clone();
//         let h = handler.as_mut().unwrap();
//         return h.call(req);
//       }

//       if handler.is_none() {
//         let allowed = match_result.leaf_handler.keys().cloned().collect();
//         if let Some(handle_method_not_allowed) = self.handle_method_not_allowed.as_mut() {
//           req.extensions_mut().insert(AllowedMethod(allowed));
//           return handle_method_not_allowed.call(req);
//         } else {
//           return Box::pin(default_method_not_allowed(allowed));
//         }
//       }
//     }

//     if (!match_result.is_catch_all || self.remove_catch_all_trailing_slash)
//       && has_trailing_slash != match_result.add_slash
//       && self.redirect_trailing_slash
//     {
//       let pth = if match_result.add_slash {
//         format!("{}/", path)
//       } else {
//         path
//       };

//       if handler.is_some() {
//         if let Some(rdb) = redirect_behavior {
//           return self.redirect(req, rdb, pth.into());
//         }
//       }
//     }

//     req.extensions_mut().insert(match_result.parameters.clone());
//     let fut = match_result.value.unwrap().call(req);

//     if let Some(handle_panic) = self.handle_panic.clone() {
//       let fut = AssertUnwindSafe(fut).catch_unwind();
//       Box::pin(async move {
//         match fut.await {
//           Ok(response) => response,
//           Err(panic) => handle_panic(panic).await,
//         }
//       })
//     } else {
//       Box::pin(handle_panics(fut))
//     }
//   }
// }

#[cfg(test)]
mod tests {
  use core::panic;
  use std::{
    any::type_name,
    borrow::Cow,
    collections::HashMap,
    sync::{
      atomic::{AtomicU64, Ordering},
      Arc, Mutex,
    },
  };

  use hyper::{body, header::HeaderValue, http, Body, Method, Request, Response, StatusCode};

  use percent_encoding::percent_encode;

  use crate::{RedirectBehavior, RequestExt};

  use super::{middleware_fn, AllowedMethods, Builder, ResponseResult, RouterBuilder, Treemux};

  async fn empty_ok_response(_req: Request<Body>) -> ResponseResult {
    Ok(Response::builder().status(StatusCode::OK).body(Body::empty()).unwrap())
  }

  #[test]
  #[should_panic]
  fn empty_router_mapping() {
    Builder::default().get("", empty_ok_response);
  }

  #[tokio::test]
  async fn scope_slash_mapping() {
    if std::env::var("TEST_LOG").is_ok() {
      femme::try_with_level(femme::LevelFilter::Trace).ok();
    }
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
  async fn empty_scope_mapping() {
    if std::env::var("TEST_LOG").is_ok() {
      femme::try_with_level(femme::LevelFilter::Trace).ok();
    }
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
  async fn group_method_scenarios() {
    group_methods(false).await;
    group_methods(true).await;
  }

  #[tokio::test]
  #[should_panic]
  async fn invalid_path() {
    let mut b = Builder::default();
    b.scope("foo");
  }
  #[tokio::test]
  #[should_panic]
  async fn invalid_sub_path() {
    let mut b = Builder::default();
    let g = b.scope("/foo");
    g.scope("bar");
  }

  #[tokio::test]
  async fn set_get_after_head() {
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

    let mux = Arc::new(Mutex::new(router.build()));
    let test_method = |method: Method, expect: Method| {
      let router = mux.lock();
      async move {
        let req = Request::builder()
          .uri("/abc")
          .method(method.clone())
          .body(Body::empty())
          .unwrap();
        let result = router.unwrap().serve(req).await;
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
  async fn not_found() {
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
  async fn method_not_allowed_handler() {
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
    router.method_not_allowed(|request: Request<Body>| {
      let allowed = request
        .extensions()
        .get::<AllowedMethods>()
        .map(|v| v.0.clone())
        .unwrap_or_default();
      let mut allowed = allowed.iter().map(|v| v.as_str().to_string()).collect::<Vec<String>>();
      allowed.sort();
      async move {
        Ok(
          Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(http::header::ALLOW, allowed.join(", "))
            .body(Body::from("custom method not allowed"))
            .unwrap(),
        )
      }
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
  async fn handle_options() {
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
  async fn panic() {
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
    router.handle_panics(|panic| async move {
      let error = Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .header("X-Result", HeaderValue::from_static("done"))
        .body(Body::from(format!("{:?}", panic)))
        .unwrap();
      Ok(error)
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
  async fn redirects() {
    if std::env::var("TEST_LOG").is_ok() {
      femme::try_with_level(femme::LevelFilter::Trace).ok();
    }
    use RedirectBehavior::*;
    info!("Testing with all {}", StatusCode::MOVED_PERMANENTLY);
    redirect(Redirect301, Redirect301, Redirect301, false).await;
    info!("Testing with all UseHandler");
    redirect(UseHandler, UseHandler, UseHandler, false).await;
    info!(
      "Testing with all {}, GET {}, POST UseHandler",
      StatusCode::MOVED_PERMANENTLY,
      StatusCode::TEMPORARY_REDIRECT
    );
    redirect(Redirect301, Redirect307, UseHandler, true).await;
    info!(
      "Testing with all UseHandler, GET {}, POST {}",
      StatusCode::PERMANENT_REDIRECT,
      StatusCode::TEMPORARY_REDIRECT
    );
    redirect(UseHandler, Redirect301, Redirect308, true).await;
  }

  fn redir_to_statuscode(behavior: RedirectBehavior) -> StatusCode {
    match behavior {
      RedirectBehavior::Redirect301 => StatusCode::MOVED_PERMANENTLY,
      RedirectBehavior::Redirect307 => StatusCode::TEMPORARY_REDIRECT,
      RedirectBehavior::Redirect308 => StatusCode::PERMANENT_REDIRECT,
      RedirectBehavior::UseHandler => StatusCode::NO_CONTENT,
    }
  }

  async fn redirect(
    default_behavior: RedirectBehavior,
    get_behavior: RedirectBehavior,
    post_behavior: RedirectBehavior,
    custom_methods: bool,
  ) {
    let redirect_handler = |_req| async {
      Ok(
        Response::builder()
          .status(StatusCode::NO_CONTENT)
          .body(Body::empty())
          .unwrap(),
      )
    };

    let default_sc: StatusCode = redir_to_statuscode(default_behavior);
    let get_sc: StatusCode = redir_to_statuscode(get_behavior);
    let post_sc: StatusCode = redir_to_statuscode(post_behavior);

    let mut expected_code_map = HashMap::new();
    expected_code_map.insert(Method::PUT, default_sc);

    let mut router = Treemux::builder();
    router.redirect_behavior = Some(default_behavior);
    if custom_methods {
      router.redirect_method_behavior.insert(Method::GET, get_behavior);
      router.redirect_method_behavior.insert(Method::POST, post_behavior);
      expected_code_map.insert(Method::GET, get_sc);
      expected_code_map.insert(Method::POST, post_sc);
    } else {
      expected_code_map.insert(Method::GET, default_sc);
      expected_code_map.insert(Method::POST, default_sc);
    }

    router.get("/slash/", redirect_handler);
    router.get("/noslash", redirect_handler);
    router.post("/slash/", redirect_handler);
    router.post("/noslash", redirect_handler);
    router.put("/slash/", redirect_handler);
    router.put("/noslash", redirect_handler);
    let mux = router.build();

    for (method, expected_code) in &expected_code_map {
      info!("Testing method {}, expecting code {}", method, expected_code);

      let req = Request::builder()
        .method(method)
        .uri("/slash")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(expected_code, &res.status());
      assert!(
        (expected_code != &StatusCode::NO_CONTENT
          && res.headers().get("Location").map(|v| v.to_str().unwrap()) == Some("/slash/"))
          || expected_code == &StatusCode::NO_CONTENT
      );

      let req = Request::builder()
        .method(method)
        .uri("/noslash/")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(expected_code, &res.status());
      assert!(
        (expected_code != &StatusCode::NO_CONTENT
          && res.headers().get("Location").map(|v| v.to_str().unwrap()) == Some("/noslash"))
          || expected_code == &StatusCode::NO_CONTENT
      );

      let req = Request::builder()
        .method(method)
        .uri("/noslash")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(StatusCode::NO_CONTENT, res.status());

      let req = Request::builder()
        .method(method)
        .uri("/noslash?a=1&b=2")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(StatusCode::NO_CONTENT, res.status());

      let req = Request::builder()
        .method(method)
        .uri("/slash/")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(StatusCode::NO_CONTENT, res.status());

      let req = Request::builder()
        .method(method)
        .uri("/slash/?a=1&b=2")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(StatusCode::NO_CONTENT, res.status());

      let req = Request::builder()
        .method(method)
        .uri("/slash?a=1&b=2")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(expected_code, &res.status());
      assert!(
        (expected_code != &StatusCode::NO_CONTENT
          && res.headers().get("Location").map(|v| v.to_str().unwrap()) == Some("/slash/?a=1&b=2"))
          || expected_code == &StatusCode::NO_CONTENT,
        "/slash?a=1&b=2 was redirected to {}",
        res
          .headers()
          .get("Location")
          .map(|v| v.to_str().unwrap())
          .unwrap_or("<no location>"),
      );

      let req = Request::builder()
        .method(method)
        .uri("/noslash/?a=1&b=2")
        .body(Body::empty())
        .unwrap();
      let res = mux.serve(req).await.unwrap();
      assert_eq!(expected_code, &res.status());
      assert!(
        (expected_code != &StatusCode::NO_CONTENT
          && res.headers().get("Location").map(|v| v.to_str().unwrap()) == Some("/noslash?a=1&b=2"))
          || expected_code == &StatusCode::NO_CONTENT,
        "/noslash/?a=1&b=2 was redirected to {}",
        res
          .headers()
          .get("Location")
          .map(|v| v.to_str().unwrap())
          .unwrap_or("<no location>"),
      );
    }
  }

  #[tokio::test]
  async fn skip_redirect() {
    let mut router = Treemux::builder();
    router.redirect_trailing_slash = false;
    router.redirect_clean_path = false;
    router.get("/slash/", empty_ok_response);
    router.get("/noslash", empty_ok_response);

    let mux = router.build();
    let req = Request::builder().uri("/slash").body(Body::empty()).unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::NOT_FOUND, resp.status());

    let req = Request::builder().uri("/noslash/").body(Body::empty()).unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::NOT_FOUND, resp.status());
  }

  #[tokio::test]
  async fn catch_all_trailing_slash() {
    let redirect_settings = vec![false, true];

    let test_path = |mux: Arc<Treemux>, pth: Cow<'static, str>| async move {
      let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/abc/{}", pth))
        .body(Body::empty())
        .unwrap();

      let tm = mux.clone();
      let resp = tm.serve(req).await.unwrap();
      let trailing_slash = pth.ends_with('/');

      let expected_code = if trailing_slash && tm.redirect_trailing_slash && tm.remove_catch_all_trailing_slash {
        StatusCode::MOVED_PERMANENTLY
      } else {
        StatusCode::OK
      };
      assert_eq!(expected_code, resp.status());
    };

    for redirect_trailing_slash in &redirect_settings {
      for remove_catch_all_slash in &redirect_settings {
        let mut router = Treemux::builder();
        router.remove_catch_all_trailing_slash = *remove_catch_all_slash;
        router.redirect_trailing_slash = *redirect_trailing_slash;
        router.get("/abc/*path", empty_ok_response);

        let mux = Arc::new(router.build());
        test_path(mux.clone(), "apples".into()).await;
        test_path(mux.clone(), "apples/".into()).await;
        test_path(mux.clone(), "apples/bananas".into()).await;
        test_path(mux.clone(), "apples/bananas/".into()).await;
      }
    }
  }

  #[tokio::test]
  async fn root() {
    let mut router = Treemux::builder();
    router.get("/", |_| async {
      Ok(
        Response::builder()
          .status(StatusCode::NO_CONTENT)
          .body(Body::empty())
          .unwrap(),
      )
    });
    let mux = router.build();
    let req = Request::builder()
      .method(Method::GET)
      .uri("/")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::NO_CONTENT, resp.status());
  }

  #[tokio::test]
  async fn wildcard_at_split_node() {
    let mut router = Treemux::builder();
    router.get("/pumpkin", slug_handler);
    router.get("/passing", slug_handler);
    router.get("/:slug", slug_handler);
    router.get("/:slug/abc", slug_handler);
    let mux = router.build();

    let req = Request::builder()
      .method(Method::GET)
      .uri("/patch")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("patch", &param);

    let req = Request::builder()
      .method(Method::GET)
      .uri("/patch/abc")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("patch", &param);

    let req = Request::builder()
      .method(Method::GET)
      .uri("/patch/def")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::NOT_FOUND, resp.status());
  }

  async fn slug_handler(req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let val = req.params().get("slug").unwrap_or_default().to_string();
    Ok(Response::new(Body::from(val)))
  }

  #[tokio::test]
  async fn slash() {
    let mut router = Treemux::builder();
    router.get("/abc/:param", param_handler);
    router.get("/year/:year/month/:month", ym_handler);
    let mux = router.build();

    let req = Request::builder()
      .method(Method::GET)
      .uri("/abc/de%2ff")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("de/f", &param);

    let req = Request::builder()
      .method(Method::GET)
      .uri("/year/de%2f/month/fg%2f")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("de/ fg/", &param);
  }

  #[tokio::test]
  async fn query_string() {
    let mut router = Treemux::builder();
    router.get("/static", param_handler);
    router.get("/wildcard/:param", param_handler);
    router.get("/catchall/*param", param_handler);

    let mux = router.build();

    let req = Request::builder()
      .method(Method::GET)
      .uri("/static?abc=def&ghi=jkl")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("", &param);

    let req = Request::builder()
      .method(Method::GET)
      .uri("/wildcard/aaa?abc=def")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("aaa", &param);

    let req = Request::builder()
      .method(Method::GET)
      .uri("/catchall/bbb?abc=def")
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    let param = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
      .unwrap()
      .to_string();
    assert_eq!("bbb", &param);
  }

  async fn param_handler(req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let val = req.params().get("param").unwrap_or_default().to_string();
    Ok(Response::new(Body::from(val)))
  }

  async fn ym_handler(req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let val1 = req.params().get("year").unwrap_or_default().to_string();
    let val2 = req.params().get("month").unwrap_or_default().to_string();
    Ok(Response::new(Body::from(format!("{} {}", val1, val2))))
  }

  pub const PATH_ENCODING: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'-')
    .add(b'.')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'_')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'~');

  #[tokio::test]
  async fn redirect_escaped_path() {
    let mut router = Treemux::builder();
    router.get("/:escaped/", empty_ok_response);
    let mux = router.build();

    let req = Request::builder()
      .method(Method::GET)
      .uri(format!(
        "/{}",
        percent_encode("Test P@th".as_bytes(), PATH_ENCODING).to_string()
      ))
      .body(Body::empty())
      .unwrap();
    let resp = mux.serve(req).await.unwrap();
    assert_eq!(StatusCode::MOVED_PERMANENTLY, resp.status());
    let location = resp
      .headers()
      .get(http::header::LOCATION)
      .map(|v| v.to_str().unwrap().to_string())
      .unwrap();
    assert_eq!("/Test%20P@th/", &location);
  }

  #[tokio::test]
  async fn escaped_routes() {
    let test_cases = vec![
      ("/abc/def", "/abc/def", "", ""),
      ("/abc/*star", "/abc/defg", "star", "defg"),
      ("/abc/extrapath/*star", "/abc/extrapath/*lll", "star", "*lll"),
      ("/abc/\\*def", "/abc/*def", "", ""),
      ("/abc/\\\\*def", "/abc/\\*def", "", ""),
      ("/:wild/def", "/*abcd/def", "wild", "*abcd"),
      ("/\\:wild/def", "/:wild/def", "", ""),
      ("/\\\\:wild/def", "/\\:wild/def", "", ""),
      ("/\\*abc/def", "/*abc/def", "", ""),
    ];
    let escape_routes = vec![false, true];

    for escape in escape_routes {
      let mut router = Treemux::builder();
      router.escape_added_routes = escape;

      for (route, _, _, _) in &test_cases {
        router.get(*route, |req| async move {
          let param = req.params().first();
          if let Some(param) = param {
            let v = format!("{}={}", param.key, param.value);
            Ok(Response::builder().status(StatusCode::OK).body(Body::from(v)).unwrap())
          } else {
            Ok(Response::builder().status(StatusCode::OK).body(Body::empty()).unwrap())
          }
        });
      }

      let mux = router.build();
      for (_, path, param, param_value) in &test_cases {
        let uri = hyper::Uri::builder().path_and_query(*path).build().unwrap();
        let escaped_path = uri.path().to_string();
        let escaped_is_same = escaped_path == *path;

        let req = Request::builder()
          .method(Method::GET)
          .uri(uri)
          .body(Body::empty())
          .unwrap();
        let resp = mux.serve(req).await.unwrap();
        let pv = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
          .unwrap()
          .to_string();
        if !param.is_empty() {
          assert_eq!(format!("{}={}", param, param_value), pv);
        } else {
          assert!(param.is_empty());
        }

        if !escaped_is_same {
          let req = Request::builder()
            .method(Method::GET)
            .uri(escaped_path)
            .body(Body::empty())
            .unwrap();
          let resp = mux.serve(req).await.unwrap();
          let status = resp.status();
          let pv = std::str::from_utf8(&body::to_bytes(resp.into_body()).await.unwrap())
            .unwrap_or_default()
            .to_string();
          if mux.escape_added_routes {
            assert_eq!(StatusCode::OK, status);
            assert_eq!(format!("{}={}", param, param_value), pv);
          } else {
            assert!(pv.is_empty());
            assert_eq!(StatusCode::NOT_FOUND, status);
          }
        }
      }
    }
  }

  #[tokio::test]
  async fn test_middlewares() {
    if std::env::var("TEST_LOG").is_ok() {
      femme::try_with_level(femme::LevelFilter::Trace).ok();
    }
    let counter = Arc::new(AtomicU64::new(0));
    let mut mux = Treemux::builder();

    mux.not_found(|_req| async {
      Ok(
        Response::builder()
          .status(StatusCode::NOT_FOUND)
          .body(Body::empty())
          .unwrap(),
      )
    });

    let clc = counter.clone();
    let mwfn = middleware_fn(move |next| {
      let counter = clc.clone();
      move |req| {
        let fut = next(req);
        let counter = counter.clone();
        async move {
          counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
          fut.await
        }
      }
    });
    info!("{:?}", type_name_of_val(&mwfn));
    let mux = mux.middleware(mwfn);

    let clc = counter.clone();
    mux.add_route(Method::GET, "/second", move |_req| {
      let counter = clc.clone();
      async move {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(
          Response::builder()
            .status(StatusCode::OK)
            .body("/second".into())
            .unwrap(),
        )
      }
    });
    let mux = mux.build();

    let resp = mux
      .serve(Request::get("/second").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    assert_eq!(body::to_bytes(resp.into_body()).await.unwrap().as_ref(), b"/second");
    assert_eq!(2, counter.load(Ordering::SeqCst));

    let resp = mux
      .serve(Request::get("/second").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    assert_eq!(body::to_bytes(resp.into_body()).await.unwrap().as_ref(), b"/second");
    assert_eq!(4, counter.load(Ordering::SeqCst));
  }

  fn type_name_of_val<T: ?Sized>(_val: &T) -> &'static str {
    type_name::<T>()
  }
  #[tokio::test]
  async fn test_not_found() {
    let mut mux = Treemux::builder();

    mux.not_found(|_req| async {
      Ok(
        Response::builder()
          .status(StatusCode::NOT_FOUND)
          .body(Body::empty())
          .unwrap(),
      )
    });

    mux.add_route(Method::GET, "/hello", |_req| async {
      Ok(
        Response::builder()
          .status(StatusCode::OK)
          .body("/hello".into())
          .unwrap(),
      )
    });
    let mux = mux.build();
    // mux.middleware(layer_fn(move |svc| svc));

    let resp = mux
      .serve(Request::get("/hello").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::OK, resp.status());
    assert_eq!(body::to_bytes(resp.into_body()).await.unwrap().as_ref(), b"/hello");

    let resp = mux
      .serve(Request::get("/baba").body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(StatusCode::NOT_FOUND, resp.status());
  }

  async fn group_methods(head_can_use_get: bool) {
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

      let b = router.scope("/base");
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
