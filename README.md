# cache-loader-async
[![Tests](https://github.com/ZeroTwo-Bot/cache-loader-async-rs/actions/workflows/rust.yml/badge.svg?branch=master&event=push)](https://github.com/ZeroTwo-Bot/cache-loader-async-rs/actions/workflows/rust.yml)

[crates.io](https://crates.io/crates/cache_loader_async)

The goal of this crate is to provide a thread-safe and easy way to access any data structure
which might is stored in a database at most once and keep it in cache for further requests.

This library is based on [tokio-rs](https://github.com/tokio-rs/tokio) and 
[futures](https://github.com/rust-lang/futures-rs).

# Usage
Using this library is as easy as that:
```rust
#[tokio::main]
async fn main() {
    let static_db: HashMap<String, u32> =
        vec![("foo".into(), 32), ("bar".into(), 64)]
            .into_iter()
            .collect();
    
    let cache = LoadingCache::new(move |key: String| {
        let db_clone = static_db.clone();
        async move {
            db_clone.get(&key).cloned().ok_or("error-message")
        }
    });

    let result = cache.get("foo".to_owned()).await.unwrap().0;

    assert_eq!(result, 32);
}
```

The LoadingCache will first try to look up the result in an internal HashMap and if it's
not found and there's no load ongoing, it will fire the load request and queue any other
get requests until the load request finishes.

# Features & Cache Backings

The cache-loader-async library currently supports two additional inbuilt backings: LRU & TTL
LRU evicts keys based on the cache maximum size, while TTL evicts keys automatically after their TTL expires.

## LRU Backing
You can use a simple pre-built LRU cache from the [lru-rs crate](https://github.com/jeromefroe/lru-rs) by enabling 
the `lru-cache` feature.

To create a LoadingCache with lru cache backing use the `with_backing` method on the LoadingCache.

```rust
async fn main() {
    let size: usize = 10;
    let cache = LoadingCache::with_backing(LruCacheBacking::new(size), move |key: String| {
        async move {
            Ok(key.to_lowercase())
        }
    });
}
```

## TTL Backing
You can use a simple pre-build TTL cache by enabling the `ttl-cache` feature. This will not require any 
additional dependencies.

To create a LoadingCache with ttl cache backing use the `with_backing` method on the LoadingCache.
```rust
async fn main() {
    let duration: Duration = Duration::from_secs(30);
    let cache = LoadingCache::with_backing(TtlCacheBacking::new(duration), move |key: String| {
        async move {
            Ok(key.to_lowercase())
        }
    });
}
```

## Own Backing

To implement an own cache backing, simply implement the public `CacheBacking` trait from the `backing` mod.

```rust
pub trait CacheBacking<K, V>
    where K: Eq + Hash + Sized + Clone + Send,
          V: Sized + Clone + Send {
    fn get(&mut self, key: &K) -> Option<&V>;
    fn set(&mut self, key: K, value: V) -> Option<V>;
    fn remove(&mut self, key: &K) -> Option<V>;
    fn contains_key(&self, key: &K) -> bool;
    fn remove_if(&mut self, predicate: Box<dyn Fn((&K, &V)) -> bool + Send + 'static>);
    fn clear(&mut self);
}
```