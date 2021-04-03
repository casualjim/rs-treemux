use futures::FutureExt;
use hyper::{Body, Request, Response, Server};

use tracing::info;
use treemux::{middleware_fn, RequestExt, RouterBuilder, Treemux};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt::init();

  let mut router = Treemux::builder().middleware(middleware_fn(move |next| {
    info!("global middleware constructor");
    move |request| {
      info!("global middleware handling request");
      next(request).map(|response| {
        info!("global middleware handled request");
        response
      })
    }
  }));
  router.get("/docs", |_req| async {
    Ok(Response::new("This displays some docs".into()))
  });

  // build the /api group
  let mut api_group = router.scope("/api").middleware(middleware_fn(move |next| {
    info!("/api middleware constructor");
    move |request| {
      info!("/api middleware handling request");
      next(request).map(|response| {
        info!("/api middleware handled request");
        response
      })
    }
  }));
  api_group.get("/todos", |_req| async {
    Ok(Response::new("return list of todos".into()))
  });
  api_group.post("/todos", |_req| async { Ok(Response::new("create a todo".into())) });
  api_group.get("/todos/:id", |req: Request<Body>| async move {
    let body = format!(
      "{} {} id={}",
      req.method(),
      req.route(),
      req.params().get("id").unwrap().to_string()
    );
    Ok(Response::new(body.into()))
  });

  let _server = Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await;
  Ok(())
}
