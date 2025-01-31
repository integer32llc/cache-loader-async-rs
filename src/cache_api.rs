use tokio::task::JoinHandle;
use std::hash::Hash;
use futures::Future;
use thiserror::Error;
use crate::internal_cache::{CacheAction, InternalCacheStore, CacheMessage};
use crate::backing::{CacheBacking, HashMapBacking};
use std::fmt::Debug;

#[derive(Error, Debug)]
pub enum CacheLoadingError<E: Debug> {
    #[error(transparent)]
    CommunicationError(CacheCommunicationError),
    #[error("No data found")]
    NoData(),
    // todo better handling here? eventually return loadingerror if possible
    #[error("An error occurred when loading the entity from the loader function")]
    LoadingError(E),
}

#[derive(Error, Debug)]
pub enum CacheCommunicationError {
    #[error("An error occurred when trying to submit the cache request")]
    TokioMpscSendError(),
    #[error("An error occurred when trying to join the result future")]
    FutureJoinError(#[from] tokio::task::JoinError),
    #[error("An error occurred when waiting for the broadcaster response")]
    TokioBroadcastRecvError(#[from] tokio::sync::broadcast::error::RecvError),
    #[error("An error occurred when receiving the response")]
    TokioOneshotRecvError(#[from] tokio::sync::oneshot::error::RecvError),
    #[error("Lookups are looping, internal error")]
    LookupLoop(),
}

impl<E: Debug> CacheLoadingError<E> {
    pub fn as_loading_error(&self) -> Option<&E> {
        match self {
            CacheLoadingError::LoadingError(error) => Some(error),
            _ => None
        }
    }

    pub fn into_loading_error(self) -> Option<E> {
        match self {
            CacheLoadingError::LoadingError(error) => Some(error),
            _ => None
        }
    }

    pub fn as_communication_error(&self) -> Option<&CacheCommunicationError> {
        match self {
            CacheLoadingError::CommunicationError(error) => Some(error),
            _ => None
        }
    }

    pub fn into_communication_error(self) -> Option<CacheCommunicationError> {
        match self {
            CacheLoadingError::CommunicationError(error) => Some(error),
            _ => None
        }
    }
}

#[derive(Clone)]
pub struct ResultMeta<V> {
    pub result: V,
    pub cached: bool,
}

#[derive(Debug, Clone)]
pub enum CacheEntry<V, E: Debug> {
    Loaded(V),
    Loading(tokio::sync::broadcast::Sender<Result<V, E>>),
}

#[derive(Debug)]
pub enum CacheResult<V, E: Debug> {
    Found(V),
    Loading(JoinHandle<Result<V, CacheLoadingError<E>>>),
    None,
}

#[derive(Debug, Clone)]
pub struct LoadingCache<K, V, E: Debug> {
    tx: tokio::sync::mpsc::Sender<CacheMessage<K, V, E>>
}

impl<
    K: Eq + Hash + Clone + Send + 'static,
    V: Clone + Sized + Send + 'static,
    E: Clone + Sized + Send + Debug + 'static,
> LoadingCache<K, V, E> {
    /// Creates a new instance of a LoadingCache with the default `HashMapBacking`
    ///
    /// # Arguments
    ///
    /// * `loader` - A function which returns a Future<Output=Option<V>>
    ///
    /// # Return Value
    ///
    /// This method returns a tuple, with:
    /// 0 - The instance of the LoadingCache
    /// 1 - The CacheHandle which is a JoinHandle<()> and represents the task which operates
    ///     the cache
    ///
    /// # Examples
    ///
    /// ```
    /// use cache_loader_async::cache_api::LoadingCache;
    /// use std::collections::HashMap;
    /// async fn example() {
    ///     let static_db: HashMap<String, u32> =
    ///         vec![("foo".into(), 32), ("bar".into(), 64)]
    ///             .into_iter()
    ///             .collect();
    ///
    ///     let cache = LoadingCache::new(move |key: String| {
    ///         let db_clone = static_db.clone();
    ///         async move {
    ///             db_clone.get(&key).cloned().ok_or(1)
    ///         }
    ///     });
    ///
    ///     let result = cache.get("foo".to_owned()).await.unwrap();
    ///
    ///     assert_eq!(result, 32);
    /// }
    /// ```
    pub fn new<T, F>(loader: T) -> LoadingCache<K, V, E>
        where F: Future<Output=Result<V, E>> + Sized + Send + 'static,
              T: Fn(K) -> F + Send + 'static {
        LoadingCache::with_backing(HashMapBacking::new(), loader)
    }

    /// Creates a new instance of a LoadingCache with a custom `CacheBacking`
    ///
    /// # Arguments
    ///
    /// * `backing` - The custom backing which the cache should use
    /// * `loader` - A function which returns a Future<Output=Option<V>>
    ///
    /// # Return Value
    ///
    /// This method returns a tuple, with:
    /// 0 - The instance of the LoadingCache
    /// 1 - The CacheHandle which is a JoinHandle<()> and represents the task which operates
    ///     the cache
    ///
    /// # Examples
    ///
    /// ```
    /// use cache_loader_async::cache_api::LoadingCache;
    /// use std::collections::HashMap;
    /// use cache_loader_async::backing::HashMapBacking;
    /// async fn example() {
    ///     let static_db: HashMap<String, u32> =
    ///         vec![("foo".into(), 32), ("bar".into(), 64)]
    ///             .into_iter()
    ///             .collect();
    ///
    ///     let cache = LoadingCache::with_backing(
    ///         HashMapBacking::new(), // this is the default implementation of `new`
    ///         move |key: String| {
    ///             let db_clone = static_db.clone();
    ///             async move {
    ///                 db_clone.get(&key).cloned().ok_or(1)
    ///             }
    ///         }
    ///     );
    ///
    ///     let result = cache.get("foo".to_owned()).await.unwrap();
    ///
    ///     assert_eq!(result, 32);
    /// }
    /// ```
    pub fn with_backing<T, F, B>(backing: B, loader: T) -> LoadingCache<K, V, E>
        where F: Future<Output=Result<V, E>> + Sized + Send + 'static,
              T: Fn(K) -> F + Send + 'static,
              B: CacheBacking<K, CacheEntry<V, E>> + Send + 'static {
        let (tx, rx) = tokio::sync::mpsc::channel(128);
        let store = InternalCacheStore::new(backing, tx.clone(), loader);
        store.run(rx); // we're discarding the handle, we never do unsafe stuff, so it can't error, right?
        LoadingCache {
            tx
        }
    }

    /// Retrieves or loads the value for specified key from either cache or loader function
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be loaded
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Value of type V
    /// Err - Error of type CacheLoadingError
    pub async fn get(&self, key: K) -> Result<V, CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::Get(key)).await
            .map(|opt_result| opt_result.expect("Get should always return either V or CacheLoadingError"))
            .map(|meta| meta.result)
    }

    /// Retrieves or loads the value for specified key from either cache or loader function with
    /// meta information, i.e. if the key was loaded from cache or from the loader function
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be loaded
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Value of type ResultMeta<V>
    /// Err - Error of type CacheLoadingError
    pub async fn get_with_meta(&self, key: K) -> Result<ResultMeta<V>, CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::Get(key)).await
            .map(|opt_result| opt_result.expect("Get should always return either V or CacheLoadingError"))
    }

    /// Sets the value for specified key and bypasses eventual currently ongoing loads
    /// If a key has been set programmatically, eventual concurrent loads will not change
    /// the value of the key.
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be loaded
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Previous value of type V wrapped in an Option depending whether there was a previous
    ///      value
    /// Err - Error of type CacheLoadingError
    pub async fn set(&self, key: K, value: V) -> Result<Option<V>, CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::Set(key, value)).await
            .map(|opt_meta| opt_meta.map(|meta| meta.result))
    }

    /// Loads the value for the specified key from the cache and returns None if not present
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be loaded
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Value of type Option<V>
    /// Err - Error of type CacheLoadingError
    pub async fn get_if_present(&self, key: K) -> Result<Option<V>, CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::GetIfPresent(key)).await
            .map(|opt_meta| opt_meta.map(|meta| meta.result))
    }

    /// Checks whether a specific value is mapped for the given key
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be checked
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - bool
    /// Err - Error of type CacheLoadingError
    pub async fn exists(&self, key: K) -> Result<bool, CacheLoadingError<E>> {
        self.get_if_present(key).await
            .map(|result| result.is_some())
    }

    /// Removes a specific key-value mapping from the cache and returns the previous result
    /// if there was any or None
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be evicted
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Value of type Option<V>
    /// Err - Error of type CacheLoadingError
    pub async fn remove(&self, key: K) -> Result<Option<V>, CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::Remove(key)).await
            .map(|opt_meta| opt_meta.map(|meta| meta.result))
    }

    /// Removes all entries which match the specified predicate
    ///
    /// # Arguments
    ///
    /// * `predicate` - The predicate to test all entries against
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Nothing, the removed values are discarded
    /// Err - Error of type CacheLoadingError -> the values were not discarded
    pub async fn remove_if<P: Fn((&K, Option<&V>)) -> bool + Send + Sync + 'static>(&self, predicate: P) -> Result<(), CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::RemoveIf(Box::new(predicate))).await
            .map(|_| ())
    }

    /// Removes all entries from the underlying backing
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Nothing, the removed values are discarded
    /// Err - Error of type CacheLoadingError -> the values were not discarded
    pub async fn clear(&self) -> Result<(), CacheLoadingError<E>> {
        self.send_cache_action(CacheAction::Clear()).await
            .map(|_| ())
    }

    /// Updates a key on the cache with the given update function and returns the updated value
    ///
    /// If the key is not present yet, it'll be loaded using the loader function and will be
    /// updated once this loader function completes.
    /// In case the key was manually updated via `set` during the loader function the update will
    /// take place on the manually updated value, so user-controlled input takes precedence over
    /// the loader function
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be updated
    /// * `update_fn` - A `FnOnce(V) -> V` which has the current value as parameter and should
    ///                 return the updated value
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Value of type V which is the previously mapped value
    /// Err - Error of type CacheLoadingError
    pub async fn update<U>(&self, key: K, update_fn: U) -> Result<V, CacheLoadingError<E>>
        where U: FnOnce(V) -> V + Send + 'static {
        self.send_cache_action(CacheAction::Update(key, Box::new(update_fn), true)).await
            .map(|opt_result| opt_result.expect("Get should always return either V or CacheLoadingError"))
            .map(|meta| meta.result)
    }

    /// Updates a key on the cache with the given update function and returns the updated value if
    /// it existed
    ///
    /// If the key is not present yet, it'll be ignored. This also counts for keys in LOADING state.
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be updated
    /// * `update_fn` - A `FnOnce(V) -> V` which has the current value as parameter and should
    ///                 return the updated value
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Optional value of type V which is the previously mapped value
    /// Err - Error of type CacheLoadingError
    pub async fn update_if_exists<U>(&self, key: K, update_fn: U) -> Result<Option<V>, CacheLoadingError<E>>
        where U: FnOnce(V) -> V + Send + 'static {
        self.send_cache_action(CacheAction::Update(key, Box::new(update_fn), false)).await
            .map(|opt| opt.map(|meta| meta.result))
    }

    /// Updates a key on the cache with the given update function and returns the updated value
    ///
    /// If the key is not present yet, it'll be loaded using the loader function and will be
    /// updated once this loader function completes.
    /// In case the key was manually updated via `set` during the loader function the update will
    /// take place on the manually updated value, so user-controlled input takes precedence over
    /// the loader function
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be updated
    /// * `update_fn` - A `FnMut(&mut V) -> ()` which has the current value as parameter and should
    ///                 update it accordingly
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Value of type V
    /// Err - Error of type CacheLoadingError
    pub async fn update_mut<U>(&self, key: K, update_fn: U) -> Result<V, CacheLoadingError<E>>
        where U: FnMut(&mut V) -> () + Send + 'static {
        self.send_cache_action(CacheAction::UpdateMut(key, Box::new(update_fn), true)).await
            .map(|opt_result| opt_result.expect("Get should always return either V or CacheLoadingError"))
            .map(|meta| meta.result)
    }

    /// Updates a key on the cache with the given update function and returns the updated value if
    /// it existed
    ///
    /// If the key is not present yet, it'll be ignored.
    /// Keys in LOADING state will still be updated as they get available.
    ///
    /// # Arguments
    ///
    /// * `key` - The key which should be updated
    /// * `update_fn` - A `FnMut(&mut V) -> ()` which has the current value as parameter and should
    ///                 update it accordingly
    ///
    /// # Return Value
    ///
    /// Returns a Result with:
    /// Ok - Optional value of type V
    /// Err - Error of type CacheLoadingError
    pub async fn update_mut_if_exists<U>(&self, key: K, update_fn: U) -> Result<Option<V>, CacheLoadingError<E>>
        where U: FnMut(&mut V) -> () + Send + 'static {
        self.send_cache_action(CacheAction::UpdateMut(key, Box::new(update_fn), false)).await
            .map(|opt| opt.map(|meta| meta.result))
    }

    async fn send_cache_action(&self, action: CacheAction<K, V>) -> Result<Option<ResultMeta<V>>, CacheLoadingError<E>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        match self.tx.send(CacheMessage {
            action,
            response: tx,
        }).await {
            Ok(_) => {
                match rx.await {
                    Ok(result) => {
                        match result {
                            CacheResult::Found(value) => {
                                Ok(Some(ResultMeta {
                                    result: value,
                                    cached: true,
                                }))
                            }
                            CacheResult::Loading(handle) => {
                                match handle.await {
                                    Ok(load_result) => {
                                        load_result.map(|v| Some(ResultMeta {
                                            result: v,
                                            cached: false,
                                        }))
                                    }
                                    Err(err) => {
                                        Err(CacheLoadingError::CommunicationError(CacheCommunicationError::FutureJoinError(err)))
                                    }
                                }
                            }
                            CacheResult::None => { Ok(None) }
                        }
                    }
                    Err(err) => {
                        Err(CacheLoadingError::CommunicationError(CacheCommunicationError::TokioOneshotRecvError(err)))
                    }
                }
            }
            Err(_) => {
                Err(CacheLoadingError::CommunicationError(CacheCommunicationError::TokioMpscSendError()))
            }
        }
    }
}
