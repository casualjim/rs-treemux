use anyhow::Result;
use femme::LevelFilter;
use futures::Future;
use hyper::{header::CONTENT_TYPE, http, Response, StatusCode};
use hyper::{Body, Request, Server};
use kv_log_macro::{debug, error, info};
use std::time::SystemTime;
use treemux::{Handler, RouterBuilder, Treemux};

fn log_requests<H, R>(f: H) -> Handler
where
  H: Fn(Request<Body>) -> R + Send + Sync + 'static,
  R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
{
  Box::new(move |req: Request<Body>| {
    let start = SystemTime::now();
    debug!("Received request", {
      method: req.method().as_str(),
      path: req.uri().path_and_query().unwrap().as_str(),
      headers: log::kv::value::Value::from_debug(req.headers()),
      body: log::kv::value::Value::from_debug(req.body()),
    });

    let fut = f(req);

    Box::pin(async move {
      let result = fut.await;
      let took = SystemTime::now().duration_since(start).unwrap();
      match result {
        Ok(resp) => {
          info!(
            "Finished processing request", {
              took: log::kv::value::Value::from_debug(&took),
              status_code: resp.status().as_u16(),
              headers: log::kv::value::Value::from_debug(resp.headers()),
              body: log::kv::value::Value::from_debug(resp.body()),
            }
          );
          Ok(resp)
        }
        Err(e) => {
          error!("{}", e);
          Err(e)
        }
      }
    })
  })
}

async fn todos(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
  Response::builder()
    .status(StatusCode::OK)
    .header(CONTENT_TYPE, "application/json")
    .body(Body::from(
      r#"[{"content":"the first task"}, {"content":"the first done task","done":true}]"#,
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
  femme::with_level(LevelFilter::Debug);
  let mut router = Treemux::builder();

  router.middleware(log_requests);
  router.get("/todos", todos);

  Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await
    .unwrap();
  Ok(())
}
