use std::{fs::File, io::Write, path::PathBuf};

use bytes::{Bytes, BytesMut};
use ordered_float::NotNan;
use prost::Message;
use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use vrl::value::{KeyString, Value};
use vector_core::event::{Event, EventArray, EventMetadata, LogEvent, Metric, MetricKind, MetricValue, proto};

const SEED: [u8; 32] = [0u8; 32];
const FIXTURE_COUNT: usize = 1024;
const MAX_MAP_SIZE: usize = 4;

// When generating fixtures we need f64 values that survive a JSON round-trip
// without any loss of precision or serialization ambiguity (NaN, -0.0).
// Maps an i32 to a float with at most 4 decimal places in the range
// (-214748.3648, 214748.3647), which are all exactly representable.
fn json_safe_f64() -> impl Strategy<Value = f64> {
    any::<i32>().prop_map(|i| {
        let v = f64::from(i) / 10_000.0;
        // Rounding can produce -0.0 from small negatives; normalize to +0.0.
        if v == -0.0_f64 { 0.0 } else { v }
    })
}

fn fixture_value_strategy() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Boolean),
        any::<i64>().prop_map(Value::Integer),
        json_safe_f64().prop_map(|f| Value::from(NotNan::new(f).unwrap())),
        // Use valid UTF-8 so bytes values survive a JSON round-trip without
        // encoding issues.
        any::<String>().prop_map(|s| Value::Bytes(Bytes::from(s.into_bytes()))),
    ];

    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Boolean),
            any::<i64>().prop_map(Value::Integer),
            json_safe_f64().prop_map(|f| Value::from(NotNan::new(f).unwrap())),
            any::<String>().prop_map(|s| Value::Bytes(Bytes::from(s.into_bytes()))),
            proptest::collection::vec(
                (any::<String>().prop_map(|s| KeyString::from(s.as_str())), inner.clone()),
                0..=MAX_MAP_SIZE,
            )
            .prop_map(|pairs| Value::Object(pairs.into_iter().collect())),
            proptest::collection::vec(inner, 0..=MAX_MAP_SIZE).prop_map(Value::Array),
        ]
    })
}

fn log_event_strategy() -> impl Strategy<Value = Event> {
    proptest::collection::btree_map(
        any::<String>().prop_map(|s| KeyString::from(s.as_str())),
        fixture_value_strategy(),
        0..=MAX_MAP_SIZE,
    )
    .prop_map(|map| Event::Log(LogEvent::from_map(map, EventMetadata::default())))
}

fn metric_strategy() -> impl Strategy<Value = Event> {
    let kind = prop_oneof![Just(MetricKind::Incremental), Just(MetricKind::Absolute)];
    (any::<String>(), any::<MetricValue>(), kind)
        .prop_map(|(name, value, kind)| Event::Metric(Metric::new(name, kind, value)))
}

fn event_strategy() -> impl Strategy<Value = Event> {
    prop_oneof![
        log_event_strategy(),
        metric_strategy(),
    ]
}

fn main() {
    let fixture_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../codecs/tests/data/native_encoding");
    let json_dir = fixture_dir.join("json");
    let proto_dir = fixture_dir.join("proto");
    std::fs::create_dir_all(&json_dir).unwrap();
    std::fs::create_dir_all(&proto_dir).unwrap();

    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &SEED);
    let mut runner = TestRunner::new_with_rng(Config::default(), rng);
    let strategy = event_strategy();

    for n in 0..FIXTURE_COUNT {
        let event = strategy.new_tree(&mut runner).unwrap().current();

        let mut json_out = File::create(json_dir.join(format!("{n:04}.json"))).unwrap();
        serde_json::to_writer(&mut json_out, &event).unwrap();

        let mut proto_out = File::create(proto_dir.join(format!("{n:04}.pb"))).unwrap();
        let mut buf = BytesMut::new();
        proto::EventArray::from(EventArray::from(event))
            .encode(&mut buf)
            .unwrap();
        proto_out.write_all(&buf).unwrap();
    }

    #[allow(clippy::print_stdout)]
    {
        println!("Written {FIXTURE_COUNT} fixtures to {}", fixture_dir.display());
    }
}
