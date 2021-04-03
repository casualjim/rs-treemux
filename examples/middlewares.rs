use futures::FutureExt;
use hyper::{Response, Server};
use tracing::info;
use treemux::{middleware_fn, middlewares, RouterBuilder, Treemux};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt::init();

  let mut router = Treemux::builder();
  router.get("/without", |_req| async { Ok(Response::new("no middlewares".into())) });

  let mut scoped = router.scope("/scope");
  scoped.get("/none", |_req| async {
    Ok(Response::new("no middlewares in scope".into()))
  });

  let mut router = router.middleware(middleware_fn(middlewares::log_requests));
  router.get("/only_log", |_req| async { Ok(Response::new("should log".into())) });

  let mut scoped = router.scope("/scope").middleware(middleware_fn(move |next| {
    info!("scope /scope middleware constructor");
    move |request| {
      info!("scope /scope middleware handling request");
      next(request).map(|response| {
        info!("scope /scope middleware handled request");
        response
      })
    }
  }));
  scoped.get("/depth_two", |_req| async {
    Ok(Response::new("no middlewares in scope".into()))
  });

  let mut router = router.middleware(middleware_fn(move |next| {
    info!("global middleware constructor");
    move |request| {
      info!("global middleware handling request");
      next(request).map(|response| {
        info!("global middleware handled request");
        response
      })
    }
  }));
  router.get("/stacked", |_req| async {
    Ok(Response::new("logs middleware stack output".into()))
  });
  let mut scoped = router.scope("/scope").middleware(middleware_fn(move |next| {
    info!("scope /scope middleware constructor level 2");
    move |request| {
      info!("scope /scope middleware handling request level 2");
      next(request).map(|response| {
        info!("scope /scope middleware handled request level 2");
        response
      })
    }
  }));
  scoped.get("/depth_four", |_req| async {
    Ok(Response::new("no middlewares in scope".into()))
  });

  Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await?;
  Ok(())
}
