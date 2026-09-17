pub(crate) mod env_ext {
    #[cfg(any(test, feature = "replay-engine"))]
    use super::test_env_ext;

    #[cfg(not(any(test, feature = "replay-engine")))]
    #[mutants::skip] // Covered by integration tests
    pub fn is_promise_success() -> bool {
        near_sdk::is_promise_success()
    }

    #[cfg(any(test, feature = "replay-engine"))]
    #[cfg_attr(all(feature = "replay-engine", not(test)), allow(dead_code))]
    pub fn is_promise_success() -> bool {
        test_env_ext::get_test_future_success()
    }

    #[cfg(test)]
    mod tests {
        use crate::common::env::{env_ext, test_env_ext};

        #[test]
        fn test_data_storage() {
            assert!(env_ext::is_promise_success());
            test_env_ext::set_test_future_success(false);
            assert!(!env_ext::is_promise_success());
            test_env_ext::set_test_future_success(true);
            assert!(env_ext::is_promise_success());
        }
    }
}

#[cfg(any(test, feature = "replay-engine"))]
#[cfg_attr(all(feature = "replay-engine", not(test)), allow(dead_code))]
pub(crate) mod test_env_ext {
    use std::{
        collections::BTreeMap,
        sync::{Mutex, MutexGuard},
    };

    use near_sdk::serde_json;

    use crate::common::event::EventKind;
    type ThreadId = String;
    type ValueKey = String;
    type Value = String;

    type Map = BTreeMap<ThreadId, BTreeMap<ValueKey, Value>>;

    /// This structure can store arbitrary data and link it to a particular thread.
    /// It allows the data to not be mixed in multithreaded test environment.
    struct TestDataStorage {
        data: Mutex<Map>,
    }

    static DATA: TestDataStorage = TestDataStorage {
        data: Mutex::new(BTreeMap::new()),
    };

    const FUTURE_SUCCESS_KEY: &str = "FUTURE_SUCCESS";
    const LOG_EVENTS_KEY: &str = "LOG_EVENTS";
    const EVENTS_KEY: &str = "EVENTS";

    fn data() -> MutexGuard<'static, Map> {
        DATA.data.lock().unwrap()
    }

    pub(crate) fn set_test_future_success(success: bool) {
        let mut data = data();
        let map = data.entry(thread_name()).or_default();
        map.insert(FUTURE_SUCCESS_KEY.to_owned(), success.to_string());
    }

    pub(crate) fn get_test_future_success() -> bool {
        let data = data();

        let Some(map) = data.get(&thread_name()) else {
            return true;
        };

        let Some(value) = map.get(FUTURE_SUCCESS_KEY) else {
            return true;
        };

        value.parse().unwrap()
    }

    #[mutants::skip]
    pub(crate) fn set_test_log_events(enabled: bool) {
        let mut data = data();
        let map = data.entry(thread_name()).or_default();
        map.insert(LOG_EVENTS_KEY.to_owned(), enabled.to_string());
    }

    #[mutants::skip]
    pub(crate) fn should_log_events() -> bool {
        let data = data();

        let Some(map) = data.get(&thread_name()) else {
            return true;
        };

        let Some(value) = map.get(LOG_EVENTS_KEY) else {
            return true;
        };

        value.parse().unwrap()
    }

    #[mutants::skip]
    pub(crate) fn store_event(event: &EventKind) {
        let mut data = data();
        let map = data.entry(thread_name()).or_default();

        let events = map.entry(EVENTS_KEY.to_owned()).or_default();
        let mut events: Vec<EventKind> = serde_json::from_str(events).unwrap_or_default();
        events.push(event.clone());

        map.insert(EVENTS_KEY.to_owned(), serde_json::to_string(&events).unwrap());
    }

    #[mutants::skip]
    pub(crate) fn get_events() -> Vec<EventKind> {
        let data = data();

        let Some(map) = data.get(&thread_name()) else {
            return vec![];
        };

        let Some(events) = map.get(EVENTS_KEY) else {
            return vec![];
        };

        serde_json::from_str(events).unwrap_or_default()
    }

    fn thread_name() -> String {
        std::thread::current().name().unwrap().to_owned()
    }

    #[cfg(test)]
    mod tests {
        use crate::common::env::test_env_ext;

        #[test]
        fn thread_name_test() {
            assert_eq!(
                test_env_ext::thread_name(),
                "common::env::test_env_ext::tests::thread_name_test"
            );
        }
    }
}
