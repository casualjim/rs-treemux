//! [`Router`](crate::Router) is a lightweight high performance HTTP request router.
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
//! use treemux::{Router, Params};
//! use std::convert::Infallible;
//! use hyper::{Request, Response, Body};
//! use anyhow::Error;
//!
//! async fn index(_: Request<Body>) -> Result<Response<Body>, Error> {
//!     Ok(Response::new("Hello, World!".into()))
//! }
//!
//! async fn hello(req: Request<Body>) -> Result<Response<Body>, Error> {
//!     let params = req.extensions().get::<Params>().unwrap();
//!     Ok(Response::new(format!("Hello, {}", params.by_name("user").unwrap()).into()))
//! }
//!
//! #[tokio::main]
//! async fn main() {
//!     let mut router: Router = Router::default();
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

use super::path::clean;
use anyhow::Result;
use hyper::{header, Body, Method, Request, Response, StatusCode};
use hyper::{server::conn::AddrStream, service::Service};
use matchit::{Match, Node, Params};
use std::collections::HashMap;
use std::pin::Pin;
use std::str;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::{future::Future, net::SocketAddr};

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

pub type Handler = Box<dyn Fn(Request<hyper::Body>) -> HandlerReturn + Send + Sync + 'static>;
pub type HandlerReturn = Box<dyn Future<Output = Result<Response<Body>, anyhow::Error>> + Send + 'static>;

pub struct Route {
  path: String,
  handler: Option<Handler>,
}

impl std::fmt::Debug for Route {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{{ path: {:?} }}", self.path)
  }
}

impl Route {
  fn new_with_boxed_handler<P: Into<String>>(path: P, handler: Handler) -> Route {
    let path = path.into();

    Route {
      path,
      handler: Some(handler),
    }
  }

  fn new<P, H, R>(path: P, handler: H) -> Route
  where
    P: Into<String>,
    H: Fn(Request<hyper::Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    let handler: Handler = Box::new(move |req: Request<hyper::Body>| Box::new(handler(req)));
    Route::new_with_boxed_handler(path, handler)
  }
}

/// Router dispatches requests to different handlers via configurable routes.
pub struct Router {
  trees: HashMap<Method, Node<Route>>,

  /// Enables automatic redirection if the current route can't be matched but a
  /// handler for the path with (without) the trailing slash exists.
  /// For example if `/foo/` is requested but a route only exists for `/foo`, the
  /// client is redirected to `/foo` with HTTP status code 301 for `GET` requests
  /// and 307 for all other request methods.
  pub redirect_trailing_slash: bool,

  /// If enabled, the router tries to fix the current request path, if no
  /// handle is registered for it.
  /// First superfluous path elements like `../` or `//` are removed.
  /// Afterwards the router does a case-insensitive lookup of the cleaned path.
  /// If a handle can be found for this route, the router makes a redirection
  /// to the corrected path with status code 301 for `GET` requests and 307 for
  /// all other request methods.
  /// For example `/FOO` and `/..//Foo` could be redirected to `/foo`.
  /// `redirect_trailing_slash` is independent of this option.
  pub redirect_fixed_path: bool,

  /// If enabled, the router automatically replies to `OPTIONS` requests.
  /// Custom `OPTIONS` handlers take priority over automatic replies.
  pub handle_options: bool,

  /// An optional handler that is called on automatic `OPTIONS` requests.
  /// The handler is only called if `handle_options` is true and no `OPTIONS`
  /// handler for the specific path was set.
  /// The `Allowed` header is set before calling the handler.
  handle_global_options: Option<Handler>,

  /// Configurable handler which is called when no matching route is
  /// found.
  handle_not_found: Option<Handler>,

  /// A configurable handler which is called when a request
  /// cannot be routed and `handle_method_not_allowed` is true.
  /// The `Allow` header with allowed request methods is set before the handler
  /// is called.
  handle_method_not_allowed: Option<Handler>,
}

impl Router {
  /// Insert a value into the router for a specific path indexed by a key.
  /// ```rust
  /// use treemux::Router;
  /// use hyper::{Response, Body, Method};
  ///
  /// let mut router: Router = Router::default();
  /// router.handle("/teapot", Method::GET, |_| {
  ///     async { Ok(Response::new(Body::from("I am a teapot!"))) }
  /// });
  /// ```
  pub fn handle<P, H, R>(&mut self, path: P, method: Method, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    let path = path.into();

    if !path.starts_with('/') {
      panic!("path must begin with '/' in path '{}'", path);
    }

    let route = Route::new(&path, handler);
    self
      .trees
      .entry(method)
      .or_insert_with(Node::default)
      .insert(&path, route);
  }

  /// Lookup allows the manual lookup of handler for a specific method and path.
  /// If the handler is not found, it returns a `Err(bool)` indicating whether a redirection should be performed to the same path with a trailing slash
  /// ```rust
  /// use treemux::Router;
  /// use hyper::{Response, Body, Method};
  ///
  /// let mut router = Router::default();
  /// router.get("/home", |_| {
  ///     async { Ok(Response::new(Body::from("Welcome!"))) }
  /// });
  ///
  /// let res = router.lookup(&Method::GET, "/home").unwrap();
  /// assert!(res.params.is_empty());
  /// ```
  pub fn lookup<P: AsRef<str>>(&mut self, method: &Method, path: P) -> Result<Match<Route>, bool> {
    self
      .trees
      .get(method)
      .map_or(Err(false), |n| n.match_path(path.as_ref()))
  }

  /// Register a handler for when there is no match
  pub fn not_found<H, R>(&mut self, handler: H)
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle_not_found = Some(Box::new(move |req: Request<hyper::Body>| Box::new(handler(req))));
  }

  /// Register a handler for when the path matches a different method than the requested one
  pub fn method_not_allowed<H, R>(&mut self, handler: H)
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle_method_not_allowed = Some(Box::new(move |req: Request<hyper::Body>| Box::new(handler(req))));
  }

  /// Register a handler for when the path matches a different method than the requested one
  pub fn global_options<H, R>(&mut self, handler: H)
  where
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle_global_options = Some(Box::new(move |req: Request<hyper::Body>| Box::new(handler(req))));
  }

  /// Register a handler for `GET` requests
  pub fn get<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::GET, handler);
  }

  /// Register a handler for `HEAD` requests
  pub fn head<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::HEAD, handler);
  }

  /// Register a handler for `OPTIONS` requests
  pub fn options<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::OPTIONS, handler);
  }

  /// Register a handler for `POST` requests
  pub fn post<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::POST, handler);
  }

  /// Register a handler for `PUT` requests
  pub fn put<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::PUT, handler);
  }

  /// Register a handler for `PATCH` requests
  pub fn patch<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::PATCH, handler);
  }

  /// Register a handler for `DELETE` requests
  pub fn delete<P, H, R>(&mut self, path: P, handler: H)
  where
    P: Into<String>,
    H: Fn(Request<Body>) -> R + Send + Sync + 'static,
    R: Future<Output = Result<Response<Body>>> + Send + 'static,
  {
    self.handle(path, Method::DELETE, handler);
  }

  /// Returns a list of the allowed methods for a specific path
  /// ```rust
  /// use treemux::Router;
  /// use hyper::{Response, Body, Method};
  ///
  /// let mut router = Router::default();
  /// router.get("/home", |_| {
  ///     async { Ok(Response::new(Body::from("Welcome!"))) }
  /// });
  ///
  /// let allowed = router.allowed("/home");
  /// assert!(allowed.contains(&"GET".to_string()));
  /// ```
  pub fn allowed<P: AsRef<str>>(&self, path: P) -> Vec<String> {
    let mut allowed: Vec<String> = Vec::new();
    match path.as_ref() {
      "*" => {
        for method in self.trees.keys() {
          if method != Method::OPTIONS {
            allowed.push(method.to_string());
          }
        }
      }
      _ => {
        for method in self.trees.keys() {
          if method == Method::OPTIONS {
            continue;
          }

          if let Some(tree) = self.trees.get(method) {
            let handler = tree.match_path(path.as_ref());

            if handler.is_ok() {
              allowed.push(method.to_string());
            }
          };
        }
      }
    };

    if !allowed.is_empty() {
      allowed.push(Method::OPTIONS.to_string())
    }

    allowed
  }
}

/// The default treemux configuration
impl Default for Router {
  fn default() -> Self {
    Self {
      trees: HashMap::new(),
      redirect_trailing_slash: true,
      redirect_fixed_path: true,
      handle_options: true,
      handle_global_options: None,
      handle_method_not_allowed: None,
      handle_not_found: Some(Box::new(move |_: Request<hyper::Body>| {
        Box::new(async {
          Ok(
            Response::builder()
              .status(400)
              .body(Body::from("404: Not Found"))
              .unwrap(),
          )
        })
      })),
    }
  }
}
#[doc(hidden)]
pub struct MakeRouterService<T: Send + Sync + 'static>(Arc<T>, Router);

impl<T> Service<&AddrStream> for MakeRouterService<T>
where
  T: Send + Sync + 'static,
{
  type Response = RouterService<T>;
  type Error = anyhow::Error;

  #[allow(clippy::type_complexity)]
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

  fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, conn: &AddrStream) -> Self::Future {
    let service = RouterService {
      app_context: self.0.clone(),
      router: &mut self.1,
      remote_addr: conn.remote_addr(),
    };

    let fut = async move { Ok(service) };
    Box::pin(fut)
  }
}

#[derive(Clone)]
#[doc(hidden)]
pub struct RouterService<T>
where
  T: Send + Sync + 'static,
{
  router: *mut Router,
  app_context: Arc<T>,
  remote_addr: SocketAddr,
}

impl<T> Service<Request<Body>> for RouterService<T>
where
  T: Send + Sync + 'static,
{
  type Response = Response<Body>;
  type Error = anyhow::Error;
  #[allow(clippy::type_complexity)]
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

  fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, mut req: Request<Body>) -> Self::Future {
    let router = unsafe { &mut *self.router };
    req.extensions_mut().insert(self.app_context.clone());
    req.extensions_mut().insert(self.remote_addr);
    let fut = router.serve(req);
    Box::pin(fut)
  }
}

unsafe impl<T: Send + Sync + 'static> Send for RouterService<T> {}
unsafe impl<T: Send + Sync + 'static> Sync for RouterService<T> {}

impl Router {
  pub fn new() -> Self {
    Router::default()
  }

  /// Converts the `Router` into a `Service` which you can serve directly with `Hyper`.
  /// If you have an existing `Service` that you want to incorporate a `Router` into, see
  /// [`Router::serve`](crate::Router::serve).
  /// ```rust,no_run
  /// # use treemux::Router;
  /// # use std::convert::Infallible;
  /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
  /// // Our router...
  /// let router = Router::default();
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

  /// Converts the `Router` into a `Service` which you can serve directly with `Hyper`.
  /// If you have an existing `Service` that you want to incorporate a `Router` into, see
  /// [`Router::serve`](crate::Router::serve).
  /// ```rust,no_run
  /// # use treemux::Router;
  /// # use std::convert::Infallible;
  /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
  /// // Our router...
  /// let router = Router::default();
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
  /// [`Router::into_service`](crate::Router::into_service). However, it may be useful when
  /// incorporating the router into a larger service.
  /// ```rust,no_run
  /// # use treemux::Router;
  /// # use hyper::service::{make_service_fn, service_fn};
  /// # use hyper::{Request, Body, Server};
  /// # use std::convert::Infallible;
  /// # use std::sync::Arc;
  ///
  /// # async fn run() {
  /// let mut router: Router = Router::default();
  ///
  /// let router = Arc::new(router);
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
  pub async fn serve(&self, mut req: Request<Body>) -> Result<Response<Body>> {
    let root = self.trees.get(req.method());
    let path = req.uri().path();
    if let Some(root) = root {
      match root.match_path(path) {
        Ok(lookup) => {
          req.extensions_mut().insert(lookup.params);
          let handler = lookup.value.handler.as_ref().unwrap();
          return Pin::from(handler(req)).await;
        }
        Err(tsr) => {
          if req.method() != Method::CONNECT && path != "/" {
            let code = match *req.method() {
              // Moved Permanently, request with GET method
              Method::GET => StatusCode::MOVED_PERMANENTLY,
              // Permanent Redirect, request with same method
              _ => StatusCode::PERMANENT_REDIRECT,
            };

            if tsr && self.redirect_trailing_slash {
              let path = if path.len() > 1 && path.ends_with('/') {
                path[..path.len() - 1].to_string()
              } else {
                path.to_string() + "/"
              };

              return Ok(
                Response::builder()
                  .header(header::LOCATION, path.as_str())
                  .status(code)
                  .body(Body::empty())
                  .unwrap(),
              );
            };

            if self.redirect_fixed_path {
              if let Some(fixed_path) = root.find_case_insensitive_path(&clean(path), self.redirect_trailing_slash) {
                return Ok(
                  Response::builder()
                    .header(header::LOCATION, fixed_path.as_str())
                    .status(code)
                    .body(Body::empty())
                    .unwrap(),
                );
              }
            };
          };
        }
      }
    };

    if req.method() == Method::OPTIONS && self.handle_options {
      let allow: Vec<String> = self
        .allowed(path)
        .into_iter()
        .filter(|v| !v.trim().is_empty())
        .collect();

      if !allow.is_empty() {
        match self.handle_global_options.as_ref() {
          Some(handler) => return Pin::from(handler(req)).await,
          None => {
            return Ok(
              Response::builder()
                .header(header::ALLOW, allow.join(", "))
                .body(Body::empty())
                .unwrap(),
            );
          }
        };
      }
    } else {
      // handle method not allowed
      let allow = self.allowed(path).join(", ");

      if !allow.is_empty() {
        if let Some(handler) = self.handle_method_not_allowed.as_ref() {
          return Pin::from(handler(req)).await;
        }
        return Ok(
          Response::builder()
            .header(header::ALLOW, allow)
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Body::empty())
            .unwrap(),
        );
      }
    };

    match self.handle_not_found.as_ref() {
      Some(ref mut handler) => Pin::from(handler(req)).await,
      None => Ok(Response::builder().status(404).body(Body::empty()).unwrap()),
    }
  }
}
