use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ethereum_rust_storage::Store;
use tracing::error;

use crate::utils::{parse_json_hex, RpcErr, RpcRequest};
use crate::RpcHandler;
use rand::prelude::*;
use serde_json::{json, Value};

use super::logs::LogsFilter;

#[derive(Debug, Clone)]
pub struct NewFilterRequest {
    pub request_data: LogsFilter,
}

/// Used by the tokio runtime to clean outdated filters
/// Takes 2 arguments:
/// - filters: the filters to clean up.
/// - filter_duration: represents how many *seconds* filter can last,
///   if any filter is older than this, it will be removed.
pub fn clean_outdated_filters(filters: ActiveFilters, filter_duration: Duration) {
    let mut active_filters_guard = filters.lock().unwrap_or_else(|mut poisoned_guard| {
        error!("THREAD CRASHED WITH MUTEX TAKEN; SYSTEM MIGHT BE UNSTABLE");
        **poisoned_guard.get_mut() = HashMap::new();
        filters.clear_poison();
        poisoned_guard.into_inner()
    });

    // Keep only filters that have not expired.
    active_filters_guard
        .retain(|_, (filter_timestamp, _)| filter_timestamp.elapsed() <= filter_duration);
}
/// Maps IDs to active log filters and their timestamps.
pub type ActiveFilters = Arc<Mutex<HashMap<u64, (Instant, LogsFilter)>>>;

impl NewFilterRequest {
    pub fn parse(params: &Option<Vec<serde_json::Value>>) -> Result<Self, RpcErr> {
        let filter = LogsFilter::parse(params)?;
        Ok(NewFilterRequest {
            request_data: filter,
        })
    }

    pub fn handle(
        &self,
        storage: ethereum_rust_storage::Store,
        filters: ActiveFilters,
    ) -> Result<serde_json::Value, crate::utils::RpcErr> {
        let from = self
            .request_data
            .from_block
            .resolve_block_number(&storage)?
            .ok_or(RpcErr::WrongParam("fromBlock".to_string()))?;
        let to = self
            .request_data
            .to_block
            .resolve_block_number(&storage)?
            .ok_or(RpcErr::WrongParam("toBlock".to_string()))?;

        if (from..=to).is_empty() {
            return Err(RpcErr::BadParams("Invalid block range".to_string()));
        }

        let id: u64 = random();
        let timestamp = Instant::now();
        let mut active_filters_guard = filters.lock().unwrap_or_else(|mut poisoned_guard| {
            error!("THREAD CRASHED WITH MUTEX TAKEN; SYSTEM MIGHT BE UNSTABLE");
            **poisoned_guard.get_mut() = HashMap::new();
            filters.clear_poison();
            poisoned_guard.into_inner()
        });

        active_filters_guard.insert(id, (timestamp, self.request_data.clone()));
        let as_hex = json!(format!("0x{:x}", id));
        Ok(as_hex)
    }

    pub fn stateful_call(
        req: &RpcRequest,
        storage: Store,
        state: ActiveFilters,
    ) -> Result<Value, RpcErr> {
        let request = Self::parse(&req.params)?;
        request.handle(storage, state)
    }
}

pub struct DeleteFilterRequest {
    pub id: u64,
}

impl DeleteFilterRequest {
    pub fn parse(params: &Option<Vec<serde_json::Value>>) -> Result<Self, RpcErr> {
        match params.as_deref() {
            Some([param]) => {
                let id = parse_json_hex(param).map_err(|_err| RpcErr::BadHexFormat(0))?;
                Ok(DeleteFilterRequest { id })
            }
            Some(_) => Err(RpcErr::BadParams(
                "Expected an array with a single hex encoded id".to_string(),
            )),
            None => Err(RpcErr::MissingParam("0".to_string())),
        }
    }

    pub fn handle(
        &self,
        _storage: ethereum_rust_storage::Store,
        filters: ActiveFilters,
    ) -> Result<serde_json::Value, crate::utils::RpcErr> {
        let mut active_filters_guard = filters.lock().unwrap_or_else(|mut poisoned_guard| {
            error!("THREAD CRASHED WITH MUTEX TAKEN; SYSTEM MIGHT BE UNSTABLE");
            **poisoned_guard.get_mut() = HashMap::new();
            filters.clear_poison();
            poisoned_guard.into_inner()
        });
        match active_filters_guard.remove(&self.id) {
            Some(_) => Ok(true.into()),
            None => Ok(false.into()),
        }
    }

    pub fn stateful_call(
        req: &RpcRequest,
        storage: ethereum_rust_storage::Store,
        filters: ActiveFilters,
    ) -> Result<serde_json::Value, crate::utils::RpcErr> {
        let request = Self::parse(&req.params)?;
        request.handle(storage, filters)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use crate::{
        eth::logs::{AddressFilter, LogsFilter, TopicFilter},
        map_http_requests,
        utils::test_utils::start_test_api,
        FILTER_DURATION,
    };
    use crate::{
        types::block_identifier::BlockIdentifier,
        utils::{test_utils::example_p2p_node, RpcRequest},
    };
    use ethereum_rust_storage::{EngineType, Store};
    use serde_json::{json, Value};

    use super::ActiveFilters;

    #[test]
    fn filter_request_smoke_test_valid_params() {
        let filter_req_params = json!(
                {
                    "fromBlock": "0x1",
                    "toBlock": "0x2",
                    "address": null,
                    "topics": ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"]
                }
        );
        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_newFilter",
            "params":
            [
                filter_req_params.clone()
            ]
                ,"id":1
        });
        let filters = Arc::new(Mutex::new(HashMap::new()));
        let id = run_new_filter_request_test(raw_json.clone(), filters.clone());
        let filters = filters.lock().unwrap();
        assert!(filters.len() == 1);
        let (_, filter) = filters.clone().get(&id).unwrap().clone();
        assert!(matches!(filter.from_block, BlockIdentifier::Number(1)));
        assert!(matches!(filter.to_block, BlockIdentifier::Number(2)));
        assert!(filter.address_filters.is_none());
        assert!(matches!(&filter.topics[..], [TopicFilter::Topic(_)]));
    }

    #[test]
    fn filter_request_smoke_test_valid_null_topics_null_addr() {
        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_newFilter",
            "params":
            [
                {
                    "fromBlock": "0x1",
                    "toBlock": "0xFF",
                    "topics": null,
                    "address": null
                }
            ]
                ,"id":1
        });
        let filters = Arc::new(Mutex::new(HashMap::new()));
        let id = run_new_filter_request_test(raw_json.clone(), filters.clone());
        let filters = filters.lock().unwrap();
        assert!(filters.len() == 1);
        let (_, filter) = filters.clone().get(&id).unwrap().clone();
        assert!(matches!(filter.from_block, BlockIdentifier::Number(1)));
        assert!(matches!(filter.to_block, BlockIdentifier::Number(255)));
        assert!(filter.address_filters.is_none());
        assert!(matches!(&filter.topics[..], []));
    }

    #[test]
    fn filter_request_smoke_test_valid_addr_topic_null() {
        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_newFilter",
            "params":
            [
                {
                    "fromBlock": "0x1",
                    "toBlock": "0xFF",
                    "topics": null,
                    "address": [ "0xb794f5ea0ba39494ce839613fffba74279579268" ]
                }
            ]
                ,"id":1
        });
        let filters = Arc::new(Mutex::new(HashMap::new()));
        let id = run_new_filter_request_test(raw_json.clone(), filters.clone());
        let filters = filters.lock().unwrap();
        assert!(filters.len() == 1);
        let (_, filter) = filters.clone().get(&id).unwrap().clone();
        assert!(matches!(filter.from_block, BlockIdentifier::Number(1)));
        assert!(matches!(filter.to_block, BlockIdentifier::Number(255)));
        assert!(matches!(
            filter.address_filters.unwrap(),
            AddressFilter::Many(_)
        ));
        assert!(matches!(&filter.topics[..], []));
    }

    #[test]
    #[should_panic]
    fn filter_request_smoke_test_invalid_block_range() {
        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_newFilter",
            "params":
            [
                {
                    "fromBlock": "0xFFF",
                    "toBlock": "0xA",
                    "topics": null,
                    "address": null
                }
            ]
                ,"id":1
        });
        run_new_filter_request_test(raw_json.clone(), Default::default());
    }

    #[test]
    #[should_panic]
    fn filter_request_smoke_test_from_block_missing() {
        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_newFilter",
            "params":
            [
                {
                    "fromBlock": null,
                    "toBlock": "0xA",
                    "topics": null,
                    "address": null
                }
            ]
                ,"id":1
        });
        let filters = Arc::new(Mutex::new(HashMap::new()));
        run_new_filter_request_test(raw_json.clone(), filters.clone());
    }

    fn run_new_filter_request_test(
        json_req: serde_json::Value,
        filters_pointer: ActiveFilters,
    ) -> u64 {
        let node = example_p2p_node();
        let request: RpcRequest = serde_json::from_value(json_req).expect("Test json is incorrect");
        let response = map_http_requests(
            &request,
            Store::new("in-mem", EngineType::InMemory).unwrap(),
            node,
            filters_pointer.clone(),
        )
        .unwrap()
        .to_string();
        let trimmed_id = response.trim().trim_matches('"');
        assert!(trimmed_id.starts_with("0x"));
        let hex = trimmed_id.trim_start_matches("0x");
        let parsed = u64::from_str_radix(hex, 16);
        assert!(u64::from_str_radix(hex, 16).is_ok());
        parsed.unwrap()
    }

    #[test]
    fn install_filter_removed_correctly_test() {
        let uninstall_filter_req: RpcRequest = serde_json::from_value(json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_uninstallFilter",
            "params":
            [
                "0xFF"
            ]
                ,"id":1
        }))
        .expect("Json for test is not a valid request");
        let filter = (
            0xFF,
            (
                Instant::now(),
                LogsFilter {
                    from_block: BlockIdentifier::Number(1),
                    to_block: BlockIdentifier::Number(2),
                    address_filters: None,
                    topics: vec![],
                },
            ),
        );
        let active_filters = Arc::new(Mutex::new(HashMap::from([filter])));
        map_http_requests(
            &uninstall_filter_req,
            Store::new("in-mem", EngineType::InMemory).unwrap(),
            example_p2p_node(),
            active_filters.clone(),
        )
        .unwrap();
        assert!(
            active_filters.clone().lock().unwrap().len() == 0,
            "Expected filter map to be empty after request"
        );
    }

    #[test]
    fn removing_non_existing_filter_returns_false() {
        let uninstall_filter_req: RpcRequest = serde_json::from_value(json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_uninstallFilter",
            "params":
            [
                "0xFF"
            ]
                ,"id":1
        }))
        .expect("Json for test is not a valid request");
        let active_filters = Arc::new(Mutex::new(HashMap::new()));
        let res = map_http_requests(
            &uninstall_filter_req,
            Store::new("in-mem", EngineType::InMemory).unwrap(),
            example_p2p_node(),
            active_filters.clone(),
        )
        .unwrap();
        assert!(matches!(res, serde_json::Value::Bool(false)));
    }

    #[tokio::test]
    async fn background_job_removes_filter_smoke_test() {
        // Start a test server to start the cleanup
        // task in the background
        let server_handle = tokio::spawn(async move { start_test_api().await });

        // Give the server some time to start
        tokio::time::sleep(Duration::from_secs(1)).await;

        // Install a filter through the endpiont
        let client = reqwest::Client::new();
        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_newFilter",
            "params":
            [
                {
                    "fromBlock": "0x1",
                    "toBlock": "0xA",
                    "topics": null,
                    "address": null
                }
            ]
                ,"id":1
        });
        let response: Value = client
            .post("http://localhost:8500")
            .json(&raw_json)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert!(
            response.get("result").is_some(),
            "Response should have a 'result' field"
        );

        let raw_json = json!(
        {
            "jsonrpc":"2.0",
            "method":"eth_uninstallFilter",
            "params":
            [
                response.get("result").unwrap()
            ]
                ,"id":1
        });

        tokio::time::sleep(FILTER_DURATION).await;
        tokio::time::sleep(FILTER_DURATION).await;

        let response: serde_json::Value = client
            .post("http://localhost:8500")
            .json(&raw_json)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert!(
            matches!(
                response.get("result").unwrap(),
                serde_json::Value::Bool(false)
            ),
            "Filter was expected to be deleted by background job, but it still exists"
        );

        server_handle.abort();
    }
}
