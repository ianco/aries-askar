use sqlx::{
    postgres::{PgConnection, Postgres},
    Connection, Database, TransactionManager,
};
use std::time::Duration;

use super::provision::{init_db, reset_db, PostgresStoreOptions};
use super::PostgresStore;
use crate::db_utils::{init_keys, random_profile_name};
use crate::error::Result;
use crate::future::{block_on, unblock};
use crate::keys::{
    wrap::{generate_raw_wrap_key, WrapKeyMethod},
    KeyCache,
};
use crate::store::Store;

pub struct TestDB {
    inst: Option<Store<PostgresStore>>,
    lock_txn: Option<PgConnection>,
}

impl TestDB {
    #[allow(unused)]
    pub async fn provision() -> Result<TestDB> {
        let path = match std::env::var("POSTGRES_URL") {
            Ok(p) if !p.is_empty() => p,
            _ => panic!("'POSTGRES_URL' must be defined"),
        };

        let key = generate_raw_wrap_key(None)?;
        let (store_key, enc_store_key, wrap_key, wrap_key_ref) =
            unblock(|| init_keys(WrapKeyMethod::RawKey, key)).await?;
        let default_profile = random_profile_name();

        let opts = PostgresStoreOptions::new(path.as_str())?;
        let conn_pool = opts.create_db_pool().await?;

        // we hold a transaction open with a fixed advisory lock value.
        // this will block until any existing TestDB instance is dropped
        let lock_txn = loop {
            // acquire a new connection free from the pool. this is to ensure that
            // connections are being closed, in case postgres is near the
            // configured connection limit.
            let mut lock_txn = conn_pool.acquire().await?.release();
            <Postgres as Database>::TransactionManager::begin(&mut lock_txn).await?;
            if sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(99999)")
                .fetch_one(&mut lock_txn)
                .await?
            {
                break lock_txn;
            }
            lock_txn.close().await?;
            async_std::task::sleep(Duration::from_millis(50)).await;
        };

        let mut init_txn = conn_pool.begin().await?;
        // delete existing tables
        reset_db(&mut *init_txn).await?;

        // create tables and add default profile
        let profile_id = init_db(init_txn, &default_profile, wrap_key_ref, enc_store_key).await?;

        let mut key_cache = KeyCache::new(wrap_key);
        key_cache.add_profile_mut(default_profile.clone(), profile_id, store_key);
        let inst = Store::new(PostgresStore::new(
            conn_pool,
            default_profile,
            key_cache,
            opts.host,
            opts.name,
        ));

        Ok(TestDB {
            inst: Some(inst),
            lock_txn: Some(lock_txn),
        })
    }
}

impl std::ops::Deref for TestDB {
    type Target = Store<PostgresStore>;

    fn deref(&self) -> &Self::Target {
        self.inst.as_ref().unwrap()
    }
}

impl Drop for TestDB {
    fn drop(&mut self) {
        if let Some(lock_txn) = self.lock_txn.take() {
            block_on(lock_txn.close()).expect("Error closing database connection");
        }
        if let Some(inst) = self.inst.take() {
            block_on(async_std::future::timeout(
                Duration::from_secs(30),
                inst.close(),
            ))
            .expect("Timed out waiting for the pool connection to close")
            .expect("Error closing connection pool");
        }
    }
}
