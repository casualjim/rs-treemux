use std::{
  convert::Infallible,
  net::SocketAddr,
  pin::Pin,
  sync::Arc,
  task::{Context, Poll},
};

use futures::Future;
use hyper::{http, service::Service};
use hyper::{server::conn::AddrStream, Body, Request, Response};

use crate::Treemux;

#[doc(hidden)]
pub struct MakeRouterService<T: Send + Sync + 'static>(pub Arc<T>, pub Treemux);

impl<T> Service<&AddrStream> for MakeRouterService<T>
where
  T: Send + Sync + 'static,
{
  type Response = RouterService<T>;
  type Error = Infallible;

  #[allow(clippy::type_complexity)]
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

  fn poll_ready(&mut self, _: &mut Context) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, conn: &AddrStream) -> Self::Future {
    let service = RouterService {
      app_context: self.0.clone(),
      router: &self.1,
      remote_addr: conn.remote_addr(),
    };

    // let service = self.1.clone();
    let fut = async move { Ok::<_, Infallible>(service) };
    Box::pin(fut)
  }
}

#[derive(Clone)]
#[doc(hidden)]
pub struct RouterService<T>
where
  T: Send + Sync + 'static,
{
  router: *const Treemux,
  app_context: Arc<T>,
  remote_addr: SocketAddr,
}

impl<T> Service<Request<Body>> for RouterService<T>
where
  T: Send + Sync + 'static,
{
  type Response = Response<Body>;
  type Error = http::Error;
  #[allow(clippy::type_complexity)]
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

  fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, mut req: Request<Body>) -> Self::Future {
    let router = unsafe { &*self.router };
    req.extensions_mut().insert(self.app_context.clone());
    req.extensions_mut().insert(self.remote_addr);
    let fut = router.serve(req);
    Box::pin(fut)
  }
}

unsafe impl<T: Send + Sync + 'static> Send for RouterService<T> {}
unsafe impl<T: Send + Sync + 'static> Sync for RouterService<T> {}
