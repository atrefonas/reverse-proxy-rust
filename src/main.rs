// #![deny(warnings)]
#[macro_use]
extern crate lazy_static;


use hyper::server::conn::AddrStream;
use hyper::{Body, Request, Response, Server, StatusCode};
use hyper::service::{service_fn, make_service_fn};
use std::{convert::Infallible, net::SocketAddr};
use std::net::IpAddr;
use tokio::sync::RwLock;
use std::time::Duration;
// use tokio_core::reactor::Core;
// use tokio_core::reactor::Handle;
// use futures::Future;
// use futures::future::Either;
// use tokio::sync::oneshot;
// use tokio::time::timeout;
use tokio::time::sleep;
use tokio::task;
use std::collections::HashMap;
use tokio::time::Instant;
use std::collections::HashSet;

// let body = reqwest::get("https://www.rust-lang.org")
//     .await?
//     .text()
//     .await?;
type Uri = String;


const MAX_CACHE_TIME_SECS: u64 = 3;
const REQ_TIMEOUT: u64 = 5;

#[derive(Clone, Debug)]
struct CachedResponse {
    body: String,
    time: Instant
}

struct AppState {
    response_cache: RwLock<HashMap<Uri,CachedResponse>>,
    is_fetching_set: RwLock<HashSet<Uri>>,
    count: RwLock<u32>
}
impl AppState {
    fn new() -> Self {
        let urlToResponse: HashMap<Uri,CachedResponse> = HashMap::new();
        let is_fetching_set: HashSet<Uri> = HashSet::new();
        let mutex = RwLock::new(urlToResponse);
        let countMutex = RwLock::new(0);
        let is_fetching_set = RwLock::new(is_fetching_set);
        AppState{response_cache: mutex, count: countMutex, is_fetching_set}
    }
    
}
lazy_static! {
    // static ref my_mutex: Mutex<i32> = Mutex::new(0i32);
    static ref APP_STATE: AppState = AppState::new();
    // static ref handle = &Core::new().unwrap().handle();
}

fn debug_request(req: Request<Body>) -> Result<Response<Body>, Infallible>  {
    let body_str = format!("{:?}", req);
    Ok(Response::new(Body::from(body_str)))
}

async fn delay() {
    // Wait randomly for between 0 and 10 seconds
    sleep(Duration::from_secs(REQ_TIMEOUT)).await;
}


async fn getCachedResponseLoop(url: &Uri) -> Option<CachedResponse> {
    loop {
        let response = getCachedResponse(url).await;
        match response {
            Some(val) => {
                return Some(val);
            },
            None => sleep(Duration::from_millis(250)).await
        }
    }
}
async fn getCachedResponseOrTimeout(url: &Uri) -> Option<CachedResponse> {
    let cached_resp_fut = getCachedResponseLoop(url);
    let sleep_statement = task::spawn(delay());
    let res = tokio::select! {
        _ = sleep_statement => None,
        resp = cached_resp_fut => resp
    };
    res
}

async fn getCachedResponse(url: &Uri) -> Option<CachedResponse> {
    let response_cache = &APP_STATE.response_cache.read().await;

    if (response_cache.contains_key(url)) {
        let cached_response = response_cache.get(url).unwrap();
        if (Instant::now().duration_since(cached_response.time) < Duration::new(MAX_CACHE_TIME_SECS, 0)) {
            println!("Cache is not old, returning {url} from cache");
            return Some(cached_response.clone());
        } else {
            let url = url.clone();
            task::spawn(async move {
                let mut response_cache = APP_STATE.response_cache.write().await;
                response_cache.remove(&url);
                println!("Cache is old, removed {url} from cache");
            });
        }
    }
    return None;
}

fn build_response(body: &String) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(String::from(body)))
        .unwrap()
}

async fn incr_count() -> u32 {
    let mut w = APP_STATE.count.write().await;
    *w += 1;
    *w
}

async fn is_fetching_uri(uri: &String) -> bool {
    let is_fetching_set = &APP_STATE.is_fetching_set.read().await;
    return is_fetching_set.contains(uri);
}

async fn set_is_fetching_uri(uri: &String, is_fetching: bool) {
    let mut is_fetching_set = APP_STATE.is_fetching_set.write().await;
    if (is_fetching) {
        is_fetching_set.insert(uri.clone());
    } else {
        is_fetching_set.remove(uri);
    }
}

async fn handle(client_ip: IpAddr, req: Request<Body>) -> Result<hyper::Response<Body>, Infallible> {
    if req.uri().path().starts_with("/escrow") {
        // will forward requests to port 13901
        println!("handing: {}", req.uri().path());
        match hyper_reverse_proxy::call(client_ip, "http://127.0.0.1:5984", req).await {
            Ok(response) => {Ok(response)}
            Err(_error) => {Ok(Response::builder()
                                  .status(StatusCode::INTERNAL_SERVER_ERROR)
                                  .body(Body::empty())
                                  .unwrap())}
        }
    } else if req.uri().path().starts_with("/slow") {
        println!("in handle");

        let count = incr_count().await;
        println!("{count}: here");
        // let timeout = tokio_core::reactor::Timeout::new(Duration::from_millis(170), &handle).unwrap();
        let uri_path = &req.uri().path().to_string();
        let cached_resp = getCachedResponse(uri_path).await;
        if cached_resp.is_some() {
            let x = cached_resp.unwrap();
            return Ok(build_response(&x.body));
        }
        
        println!("{count}: no cache found...");
        let is_fetching = is_fetching_uri(&uri_path).await;

        if (is_fetching) {
            println!("{count}: could not get write lock. waiting for read lock");
            let cached_resp = getCachedResponseOrTimeout(uri_path).await;
            println!("{count}: got read lock");
            if cached_resp.is_some() {
                let x = cached_resp.unwrap();
                return Ok(build_response(&x.body));
            } else {
                return Ok(build_response(&"Timed out while getting response".to_string()));
            }
        }
        // Not currently fetching, so try to fetch and refresh cache

        //FIXME - set is fetching
        set_is_fetching_uri(uri_path, true).await;

        let sleep_statement = task::spawn(delay());

        //let proxy_call = reqwest::get(uri); FIXME
        let proxy_call = reqwest::get(format!("http://localhost:8080{uri_path}")); //FIXME - dont hardcode to localhost

        let res = tokio::select! {
            _ = sleep_statement => {
                {Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("timed out"))
                    .unwrap())}
            },

            response = proxy_call => {
                match response {
                    Ok(response) => {
                        let proxy_text = match response.text().await {
                            Ok(p) => {p},
                            Err(_) => {return Ok(Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .body(Body::from(format!("error response when getting body text from {uri_path}")))
                                .unwrap())},
                        };

                        // Update Cache
                        let uri = uri_path.clone();
                        let c_body = proxy_text.clone();
                        // Not sure if this should be in its own task
                        task::spawn(async move {
                            println!("{count}: Updating cache!");
                            let mut response_cache = APP_STATE.response_cache.write().await;
                            let cached_resp = CachedResponse {
                                body: c_body,
                                time: Instant::now()
                            };
                            set_is_fetching_uri(&uri, false).await;
                            response_cache.insert(uri, cached_resp);
                        });
                        Ok(build_response(&proxy_text))
                    }
                    Err(_error) => {Ok(Response::builder()
                                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(Body::from("error response"))
                                        .unwrap())}
                    }
                }
        };

        res
    } else {
        debug_request(req)
    }
}

#[tokio::main]
async fn main() {
    let port = 8000;
    let bind_addr = format!("127.0.0.1:{port}");
    let addr:SocketAddr = bind_addr.parse().expect("Could not parse ip:port.");

    let make_svc = make_service_fn(|conn: &AddrStream| {
        let remote_addr = conn.remote_addr().ip();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| handle(remote_addr, req)))
        }
    });

    let server = Server::bind(&addr).serve(make_svc);

    println!("Running serverr on {:?}", addr);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}

