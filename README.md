# Treemux

[![Documentation](https://img.shields.io/badge/docs-0.4.0-4d76ae?style=for-the-badge)](https://docs.rs/treemux/0.4.0)
[![Version](https://img.shields.io/crates/v/treemux?style=for-the-badge)](https://crates.io/crates/treemux)
[![License](https://img.shields.io/crates/l/treemux?style=for-the-badge)](https://crates.io/crates/treemux)
[![Actions](https://img.shields.io/github/workflow/status/casualjim/rs-treemux/Rust/master?style=for-the-badge)](https://github.com/casualjim/rs-treemux/actions)

Treemux is a lightweight high performance HTTP request router.

This router supports variables in the routing pattern and matches against the request method. It also scales very well.

The router is optimized for high performance and a small memory footprint. It scales well even with very long paths and a large number of routes. A compressing dynamic trie (radix tree) structure is used for efficient matching.

Treemux started as a fork of [httprouter-rs](https://github.com/ibraheemdev/httprouter-rs) by @ibraheemdev. And is just a learning project for me at the moment, to get more familiar with hyper and tower services.
If you're familiar with the go world, this is the equivalent of [httptreemux](https://github.com/dimfeld/httptreemux) from @dimfeld vs [httprouter](https://github.com/julienschmidt/httprouter) from @julienschmidt. It also adds some middleware support.

## Features

**Only explicit matches:** With other routers, a requested URL path could match multiple patterns. Therefore they have some awkward pattern priority rules, like *longest match* or *first registered, first matched*. By design of this router, a request can only match exactly one or no route. As a result, there are also no unintended matches, which makes it great for SEO and improves the user experience.

**Path auto-correction:** Besides detecting the missing or additional trailing slash at no extra cost, the router can also fix wrong cases and remove superfluous path elements (like `../` or `//`). Is [CAPTAIN CAPS LOCK](http://www.urbandictionary.com/define.php?term=Captain+Caps+Lock) one of your users? Treemux can help him by making a case-insensitive look-up and redirecting him to the correct URL.

**Parameters in your routing pattern:** Stop parsing the requested URL path, just give the path segment a name and the router delivers the dynamic value to you. Because of the design of the router, path parameters are very cheap.

**High Performance:** Treemux relies on a tree structure which makes heavy use of *common prefixes*, it is basically a [radix tree](https://en.wikipedia.org/wiki/Radix_tree). This makes lookups extremely fast.

Of course you can also set **custom [`NotFound`](https://docs.rs/treemux/newest/treemux/router/struct.Router.html#structfield.not_found) and  [`MethodNotAllowed`](https://docs.rs/treemux/newest/treemux/router/struct.Router.html#structfield.method_not_allowed) handlers** , [**serve static files**](https://docs.rs/treemux/newest/treemux/router/struct.Router.html#method.serve_files), and [**automatically respond to OPTIONS requests**](https://docs.rs/treemux/newest/treemux/router/struct.Router.html#structfield.global_options)

## Usage

Here is a simple example:

```rust,no_run
use treemux::{middleware_fn, Treemux, RouterBuilder, Params, RequestExt};
use treemux::middlewares;
use std::convert::Infallible;
use hyper::{Request, Response, Body};
use hyper::http::Error;

async fn index(_: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::new("Hello, World!".into()))
}

async fn hello(req: Request<Body>) -> Result<Response<Body>, Error> {
  let user_name = req.params().get("user").unwrap();
  Ok(Response::new(format!("Hello, {}", user_name).into()))
}

#[tokio::main]
async fn main() {
  let router = Treemux::builder();
  let mut router = router.middleware(middleware_fn(middlewares::log_requests));
  router.get("/", index);
  router.get("/hello/:user", hello);

  hyper::Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await;
}
```

### Named parameters

As you can see, `:user` is a *named parameter*. The values are accessible via `req.params()` which is provided by the RequestExt trait.

Named parameters only match a single path segment:

```ignore
Pattern: /user/:user

 /user/gordon              match
 /user/you                 match
 /user/gordon/profile      no match
 /user/                    no match
```

**Note:** Since this router has only explicit matches, you can not register static routes and parameters for the same path segment. For example you can not register the patterns `/user/new` and `/user/:user` for the same request method at the same time. The routing of different request methods is independent from each other.

### Catch-All parameters

The second type are *catch-all* parameters and have the form `*name`. Like the name suggests, they match everything. Therefore they must always be at the **end** of the pattern:

```ignore
Pattern: /src/*filepath

 /src/                     match
 /src/somefile.go          match
 /src/subdir/somefile.go   match
```

## Automatic OPTIONS responses and CORS

One might wish to modify automatic responses to OPTIONS requests, e.g. to support [CORS preflight requests](https://developer.mozilla.org/en-US/docs/Glossary/preflight_request) or to set other headers. This can be achieved using the [`Router::global_options`](https://docs.rs/treemux/newest/treemux/router/struct.Router.html#structfield.global_options) handler:

```rust
use treemux::Treemux;
use hyper::{Request, Response, Body};
use hyper::http::Error;

async fn global_options(_: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::builder()
      .header("Access-Control-Allow-Methods", "Allow")
      .header("Access-Control-Allow-Origin", "*")
      .body(Body::empty())
      .unwrap())
}

fn main() {
  let mut router = Treemux::builder();
  router.global_options(global_options);
}
```

### Multi-domain / Sub-domains

Here is a quick example: Does your server serve multiple domains / hosts? You want to use sub-domains? Define a router per host!

```rust,no_run
use treemux::{Treemux, RouterBuilder};
use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Request, Response, Server, StatusCode};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

pub struct HostSwitch(HashMap<String, Treemux>);

impl HostSwitch {
  async fn serve(&self, req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let forbidden = Response::builder()
      .status(StatusCode::FORBIDDEN)
      .body(Body::empty())
      .unwrap();
    match req.headers().get("host") {
      Some(host) => match self.0.get(host.to_str().unwrap()) {
        Some(router) => router.serve(req).await,
        None => Ok(forbidden),
      },
      None => Ok(forbidden),
    }
  }
}

async fn hello(_: Request<Body>) -> Result<Response<Body>, http::Error> {
    Ok(Response::new(Body::default()))
}

#[tokio::main]
async fn main() {
  let mut router = Treemux::builder();
  router.get("/", hello);

  let mut host_switch: HostSwitch = HostSwitch(HashMap::new());
  host_switch.0.insert("example.com:12345".into(), router.into());

  let host_switch = Arc::new(host_switch);
  
  let make_svc = make_service_fn(move |_| {
    let host_switch = host_switch.clone();
    async move {
      Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
        let host_switch = host_switch.clone();
        async move { host_switch.serve(req).await }
      }))
    }
  });

  let server = Server::bind(&([127, 0, 0, 1], 3000).into())
      .serve(make_svc)
      .await;
}
```

### Not Found Handler

**NOTE: It might be required to set [`Router::method_not_allowed`](https://docs.rs/treemux/newest/treemux/router/struct.Treemux.html#structfield.handle_method_not_allowed) to `None` to avoid problems.**

You can use another handler, to handle requests which could not be matched by this router by using the [`Router::not_found`](https://docs.rs/treemux/newest/treemux/router/struct.Router.html#structfield.not_found) handler.

The `not_found` handler can for example be used to return a 404 page:

```rust
use treemux::Treemux;
use hyper::{Request, Response, Body};
use hyper::http::Error;

async fn not_found(_req: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::builder()
    .status(404)
    .body(Body::empty())
    .unwrap())
}

fn main() {
  let mut router = Treemux::builder();
  router.not_found(not_found);
}
```

### Static files

You can use the router to serve pages from a static file directory:

```rust
// TODO
```
