use std::time::SystemTime;

use futures::Future;
use hyper::{header::HeaderValue, http, Body, Request, Response};

use crate::Handler;

pub fn log_requests<H, R>(f: H) -> Handler
where
  H: Fn(Request<Body>) -> R + Send + Sync + 'static,
  R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
{
  Box::new(move |req: Request<Body>| {
    let start = SystemTime::now();
    let method = req.method().as_str().to_string();
    let path = req.uri().path_and_query().unwrap().as_str().to_string();
    // debug!("Received request", {
    //   method: req.method().as_str(),
    //   path: req.uri().path_and_query().unwrap().as_str(),
    //   headers: log::kv::value::Value::from_debug(req.headers()),
    //   body: log::kv::value::Value::from_debug(req.body()),
    // });

    let fut = f(req);

    Box::pin(async move {
      let result = fut.await;
      let took = SystemTime::now().duration_since(start).unwrap();
      match result {
        Ok(mut resp) => {
          if resp.status().is_success() {
            info!(
              "Processed request", {
                method: method,
                path: path,
                took: log::kv::value::Value::from_debug(&took),
                status_code: resp.status().as_u16(),
                headers: log::kv::value::Value::from_debug(resp.headers()),
                body: log::kv::value::Value::from_debug(resp.body()),
              }
            );
          } else {
            warn!(
              "Processed request", {
                method: method,
                path: path,
                took: log::kv::value::Value::from_debug(&took),
                status_code: resp.status().as_u16(),
                headers: log::kv::value::Value::from_debug(resp.headers()),
                body: log::kv::value::Value::from_debug(resp.body()),
              }
            );
          }
          let tstr = format!("{:?}", took);
          resp
            .headers_mut()
            .insert("x-request-duration", HeaderValue::from_str(&tstr).unwrap());
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
