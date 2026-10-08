//! Comprehensive grammar specification tests.
//!
//! These tests define the complete set of Python decorator syntaxes that
//! barca must parse. Tests marked `#[ignore]` are aspirational — they
//! document desired behavior that hasn't been implemented yet.

use barca_core::model::*;
use barca_core::parse::extract_nodes;

// ═══════════════════════════════════════════════════════════════════════════════
// 1. Basic decorator forms
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn bare_asset_no_parens() {
    let src = r#"
from barca import asset

@asset
def my_asset() -> dict:
    return {"x": 1}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind, NodeKind::Asset);
    assert_eq!(nodes[0].freshness, Freshness::Always);
}

#[test]
fn asset_empty_parens() {
    let src = r#"
from barca import asset

@asset()
def my_asset() -> dict:
    return {"x": 1}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind, NodeKind::Asset);
}

#[test]
fn sensor_decorator() {
    let src = r#"
from barca import sensor

@sensor()
def check_inbox():
    return (True, {"files": ["a.csv"]})
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind, NodeKind::Sensor);
    // Sensors default to Manual freshness
    assert_eq!(nodes[0].freshness, Freshness::Manual);
}

#[test]
fn task_decorator() {
    let src = r#"
from barca import task

@task()
def send_email():
    pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].kind, NodeKind::Task);
    assert_eq!(nodes[0].freshness, Freshness::Always);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Freshness policies
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn freshness_always_with_parens() {
    let src = r#"
from barca import asset, Always

@asset(freshness=Always())
def a(): pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].freshness, Freshness::Always);
}

#[test]
fn freshness_always_singleton_no_parens() {
    // Decision #2: support singleton form without parens
    let src = r#"
from barca import asset, Always

@asset(freshness=Always)
def a(): pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].freshness, Freshness::Always);
}

#[test]
fn freshness_manual_with_parens() {
    let src = r#"
from barca import asset, Manual

@asset(freshness=Manual())
def a(): pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].freshness, Freshness::Manual);
}

#[test]
fn freshness_manual_singleton_no_parens() {
    let src = r#"
from barca import asset, Manual

@asset(freshness=Manual)
def a(): pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].freshness, Freshness::Manual);
}

#[test]
fn freshness_schedule_cron() {
    let src = r#"
from barca import asset, Schedule

@asset(freshness=Schedule("0 5 * * *"))
def daily_job(): pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(
        nodes[0].freshness,
        Freshness::Schedule(CronExpr("0 5 * * *".into()))
    );
}

#[test]
fn freshness_schedule_accepts_6_field_seconds_cron() {
    // Sub-minute support (widening issue #109): a 6-field cron with a leading
    // seconds field is now valid — the scheduler evaluates at 1-second
    // resolution — and round-trips as-is.
    let src = r#"
from barca import asset, Schedule

@asset(freshness=Schedule("*/30 * * * * *"))
def every_30s(): pass
"#;
    let nodes = extract_nodes(src, "pipeline.py").unwrap();
    assert_eq!(
        nodes[0].freshness,
        Freshness::Schedule(CronExpr("*/30 * * * * *".into()))
    );
}

#[test]
fn freshness_schedule_rejects_7_field_year_cron() {
    // The seconds field was allowed, but the year field is still disallowed —
    // proves the grammar widened for seconds only, not everything.
    let src = r#"
from barca import asset, Schedule

@asset(freshness=Schedule("0 0 5 * * * 2026"))
def with_year(): pass
"#;
    assert!(extract_nodes(src, "pipeline.py").is_err());
}

#[test]
fn freshness_schedule_rejects_malformed_cron() {
    let src = r#"
from barca import asset, Schedule

@asset(freshness=Schedule("not a cron"))
def daily_job(): pass
"#;
    assert!(extract_nodes(src, "test.py").is_err());
}

#[test]
fn freshness_schedule_rejects_empty_cron() {
    let src = r#"
from barca import asset, Schedule

@asset(freshness=Schedule(""))
def daily_job(): pass
"#;
    assert!(extract_nodes(src, "test.py").is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Input declarations — aliasing and references
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn input_same_name_as_function() {
    let src = r#"
from barca import asset

@asset()
def raw_data(): return {}

@asset(inputs={"raw_data": raw_data})
def processed(raw_data: dict) -> dict:
    return raw_data
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[1].inputs[0].param_name, "raw_data");
    assert_eq!(
        nodes[1].inputs[0].upstream,
        NodeRef::FunctionName("raw_data".into())
    );
}

#[test]
fn input_aliased_param_name() {
    // Decision #1: param name can differ from upstream function name
    let src = r#"
from barca import asset

@asset()
def fetch_raw_prices(): return {"AAPL": 150}

@asset(inputs={"data": fetch_raw_prices})
def normalize(data: dict) -> dict:
    return data
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[1].inputs[0].param_name, "data");
    assert_eq!(
        nodes[1].inputs[0].upstream,
        NodeRef::FunctionName("fetch_raw_prices".into())
    );
}

#[test]
fn input_multiple_upstreams() {
    let src = r#"
from barca import asset

@asset()
def prices(): return {}

@asset()
def volumes(): return {}

@asset(inputs={"p": prices, "v": volumes})
def combined(p: dict, v: dict) -> dict:
    return {**p, **v}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[2].inputs.len(), 2);
    assert_eq!(nodes[2].inputs[0].param_name, "p");
    assert_eq!(nodes[2].inputs[1].param_name, "v");
}

#[test]
fn input_collect_fan_in() {
    let src = r#"
from barca import asset, collect, partitions

@asset(partitions={"ticker": partitions(["AAPL", "MSFT"])})
def fetch_prices(ticker: str) -> dict:
    return {"ticker": ticker}

@asset(inputs={"all_prices": collect(fetch_prices)})
def summary(all_prices: dict) -> dict:
    return {"count": len(all_prices)}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[1].inputs[0].param_name, "all_prices");
    assert!(nodes[1].inputs[0].collected);
    assert_eq!(
        nodes[1].inputs[0].upstream,
        NodeRef::FunctionName("fetch_prices".into())
    );
}

#[test]
fn input_canonical_asset_ref() {
    let src = r#"
from barca import asset, asset_ref

@asset(inputs={"data": asset_ref("other_module/assets.py:raw_data")})
def process(data: dict) -> dict:
    return data
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].inputs[0].param_name, "data");
    assert_eq!(
        nodes[0].inputs[0].upstream,
        NodeRef::Canonical("other_module/assets.py:raw_data".into())
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Sensor → downstream: payload unpacking
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn sensor_as_input_to_asset() {
    // Decision #4: downstream receives just the payload, not the full (bool, output) tuple.
    // The parser/DAG doesn't enforce this — it's a runner concern. But the structure is valid.
    let src = r#"
from barca import asset, sensor, Schedule, Always

@sensor(freshness=Schedule("*/5 * * * *"))
def inbox_sensor():
    return (True, {"files": ["a.csv", "b.csv"]})

@asset(inputs={"files": inbox_sensor}, freshness=Always)
def process_inbox(files: list) -> dict:
    return {"processed": len(files)}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].kind, NodeKind::Sensor);
    assert_eq!(nodes[1].kind, NodeKind::Asset);
    assert_eq!(nodes[1].inputs[0].param_name, "files");
    assert_eq!(
        nodes[1].inputs[0].upstream,
        NodeRef::FunctionName("inbox_sensor".into())
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 5. Partitions — static, dynamic, derived
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn partitions_static_string_list() {
    let src = r#"
from barca import asset, partitions

@asset(partitions={"ticker": partitions(["AAPL", "MSFT", "GOOG"])})
def fetch_prices(ticker: str) -> dict:
    return {"ticker": ticker}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let spec = nodes[0].partitions.get("ticker").unwrap();
    match spec {
        PartitionSpec::Static { values } => {
            assert_eq!(values.len(), 3);
            assert_eq!(values[0], PartitionValue::Str("AAPL".into()));
            assert_eq!(values[1], PartitionValue::Str("MSFT".into()));
            assert_eq!(values[2], PartitionValue::Str("GOOG".into()));
        }
        _ => panic!("expected static partitions"),
    }
}

#[test]
fn partitions_static_int_list() {
    let src = r#"
from barca import asset, partitions

@asset(partitions={"year": partitions([2020, 2021, 2022, 2023])})
def annual_report(year: int) -> dict:
    return {"year": year}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let spec = nodes[0].partitions.get("year").unwrap();
    match spec {
        PartitionSpec::Static { values } => {
            assert_eq!(values.len(), 4);
            // Note: int extraction from ruff AST is implementation-dependent
        }
        _ => panic!("expected static partitions"),
    }
}

#[test]
fn partitions_derived_from_upstream() {
    let src = r#"
from barca import asset, partitions_from

@asset()
def ticker_universe() -> list:
    return ["AAPL", "MSFT", "GOOG"]

@asset(partitions={"ticker": partitions_from(ticker_universe)})
def fetch_prices(ticker: str) -> dict:
    return {"ticker": ticker}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let spec = nodes[1].partitions.get("ticker").unwrap();
    match spec {
        PartitionSpec::DerivedFrom { source_ref } => {
            assert_eq!(source_ref.resolution_name(), "ticker_universe");
        }
        _ => panic!("expected derived partitions"),
    }
}

#[test]
fn partitions_list_comprehension_parsed_as_dynamic() {
    // List comprehension can't be evaluated statically — stored as Dynamic
    // for Python evaluation at plan time.
    let src = r#"
from barca import asset, partitions

@asset(partitions={"key": partitions([f"p{i:05d}" for i in range(100)])})
def wide_asset(key: str) -> dict:
    return {"key": key}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let spec = nodes[0].partitions.get("key").unwrap();
    match spec {
        PartitionSpec::Dynamic { source_text } => {
            assert!(source_text.contains("for i in range(100)"));
        }
        _ => panic!("expected Dynamic partition spec, got {spec:?}"),
    }
}

#[test]
fn partitions_function_call_parsed_as_dynamic() {
    // Function call can't be evaluated statically — stored as Dynamic.
    let src = r#"
from barca import asset, partitions

def get_tickers():
    return ["AAPL", "MSFT", "GOOG", "AMZN"]

@asset(partitions={"ticker": partitions(get_tickers())})
def fetch_prices(ticker: str) -> dict:
    return {"ticker": ticker}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let spec = nodes[0].partitions.get("ticker").unwrap();
    match spec {
        PartitionSpec::Dynamic { source_text } => {
            assert!(source_text.contains("get_tickers()"));
        }
        _ => panic!("expected Dynamic partition spec, got {spec:?}"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// 6. Sinks — stacking, serializers
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn single_sink() {
    let src = r#"
from barca import asset, sink

@asset()
@sink("output/report.json", serializer="json")
def report() -> dict:
    return {"rows": 42}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].sinks.len(), 1);
    assert_eq!(nodes[0].sinks[0].path, "output/report.json");
    assert_eq!(nodes[0].sinks[0].serializer, Some(SerializerKind::Json));
}

#[test]
fn multiple_sinks_stacked() {
    let src = r#"
from barca import asset, sink

@asset()
@sink("local/report.json", serializer="json")
@sink("s3://my-bucket/report.parquet", serializer="parquet")
@sink("tmp/report.txt", serializer="text")
def report() -> dict:
    return {"rows": 42}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].sinks.len(), 3);
    assert_eq!(nodes[0].sinks[0].serializer, Some(SerializerKind::Json));
    assert_eq!(nodes[0].sinks[1].serializer, Some(SerializerKind::Parquet));
    assert_eq!(nodes[0].sinks[2].serializer, Some(SerializerKind::Text));
}

#[test]
fn sink_without_serializer() {
    let src = r#"
from barca import asset, sink

@asset()
@sink("output/data.json")
def my_data() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].sinks.len(), 1);
    assert_eq!(nodes[0].sinks[0].serializer, None);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 7. Metadata — name, description, tags, timeout
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn explicit_name_override() {
    let src = r#"
from barca import asset

@asset(name="prices")
def fetch_latest_prices() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].explicit_name, Some("prices".into()));
    assert_eq!(nodes[0].continuity_key(), "prices");
}

#[test]
fn continuity_key_defaults_to_file_and_function() {
    let src = r#"
from barca import asset

@asset()
def my_asset() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "project/assets.py").unwrap();
    assert_eq!(nodes[0].continuity_key(), "project/assets.py:my_asset");
}

#[test]
fn description_parameter() {
    let src = r#"
from barca import asset

@asset(description="Fetches daily price data from the exchange API")
def fetch_prices() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(
        nodes[0].description,
        Some("Fetches daily price data from the exchange API".into())
    );
}

#[test]
fn tags_parameter() {
    let src = r#"
from barca import asset

@asset(tags={"team": "data-eng", "concurrency_group": "network"})
def api_call() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].tags.get("team").unwrap(), "data-eng");
    assert_eq!(nodes[0].tags.get("concurrency_group").unwrap(), "network");
}

#[test]
fn timeout_seconds_parameter() {
    let src = r#"
from barca import asset

@asset(timeout_seconds=600)
def slow_train() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].timeout_seconds, 600);
}

#[test]
fn timeout_defaults_to_300() {
    let src = r#"
from barca import asset

@asset()
def normal_asset() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].timeout_seconds, 300);
}

#[test]
fn retries_and_retry_backoff_parsed() {
    let src = r#"
from barca import asset

@asset(retries=3, retry_backoff=2.5)
def flaky() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].retries, 3);
    assert_eq!(nodes[0].retry_backoff_seconds, 2.5);
}

#[test]
fn retry_backoff_accepts_int_literal() {
    let src = r#"
from barca import asset

@asset(retries=2, retry_backoff=2)
def flaky() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes[0].retry_backoff_seconds, 2.0);
}

#[test]
fn retries_defaults_and_zero_clamps_to_one() {
    let src = r#"
from barca import asset

@asset()
def normal() -> dict:
    return {}

@asset(retries=0)
def clamped() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    // Default: 1 attempt, no backoff.
    assert_eq!(nodes[0].retries, 1);
    assert_eq!(nodes[0].retry_backoff_seconds, 0.0);
    // retries=0 is meaningless (zero attempts) → clamped to 1.
    assert_eq!(nodes[1].retries, 1);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 8. @unsafe decorator
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn unsafe_marks_function() {
    let src = r#"
from barca import asset, unsafe

@unsafe
@asset()
def dynamic_config() -> dict:
    return eval("{'key': 'value'}")
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert!(nodes[0].is_unsafe);
}

#[test]
fn unsafe_order_independent() {
    // @unsafe can come before or after @asset
    let src = r#"
from barca import asset, unsafe

@asset()
@unsafe
def dynamic_config() -> dict:
    return eval("{'key': 'value'}")
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert!(nodes[0].is_unsafe);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 9. DAG validation
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn dag_rejects_task_as_asset_input() {
    use barca_core::dag::Dag;

    let src = r#"
from barca import asset, task

@task()
def send_email():
    pass

@asset(inputs={"email": send_email})
def bad_asset(email) -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let result = Dag::build(&nodes);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("task"));
}

#[test]
fn dag_allows_asset_upstream_of_task() {
    use barca_core::dag::Dag;

    let src = r#"
from barca import asset, task

@asset()
def data() -> dict:
    return {}

@task(inputs={"d": data})
def deploy(d):
    pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert!(Dag::build(&nodes).is_ok());
}

#[test]
fn dag_rejects_sensor_with_inputs() {
    use barca_core::dag::Dag;

    let src = r#"
from barca import asset, sensor

@asset()
def data(): return {}

@sensor(inputs={"data": data})
def bad_sensor(data):
    return (True, {})
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let result = Dag::build(&nodes);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("sensor"));
}

#[test]
fn dag_rejects_duplicate_continuity_key() {
    use barca_core::dag::Dag;

    let src = r#"
from barca import asset

@asset(name="shared_name")
def asset_a(): return {}

@asset(name="shared_name")
def asset_b(): return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let result = Dag::build(&nodes);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("duplicate") || err.contains("Duplicate"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// 10. DAG shape classification
// ═══════════════════════════════════════════════════════════════════════════════
// 10. Aspirational: split plans for partitions_from
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn partitions_from_creates_partition_source_edge() {
    use barca_core::dag::Dag;

    let src = r#"
from barca import asset, partitions_from

@asset()
def ticker_universe() -> list:
    return ["AAPL", "MSFT", "GOOG"]

@asset(partitions={"ticker": partitions_from(ticker_universe)})
def fetch_prices(ticker: str) -> dict:
    return {"ticker": ticker}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let dag = Dag::build(&nodes).unwrap();

    // The partition source edge should be visible in the DAG
    let upstream = dag.upstream("test.py:fetch_prices");
    assert!(upstream.contains(&"test.py:ticker_universe"));
}

// ═══════════════════════════════════════════════════════════════════════════════
// 12. Aspirational: collect() type semantics
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn collect_marks_input_as_collected() {
    // Decision #5: collect() changes the runtime type to dict[tuple, T]
    // The parser just marks it; type checking is a stubs concern.
    let src = r#"
from barca import asset, collect, partitions

@asset(partitions={"ticker": partitions(["AAPL", "MSFT"])})
def per_ticker(ticker: str) -> dict:
    return {"ticker": ticker, "price": 100}

@asset(inputs={"all_data": collect(per_ticker)})
def aggregate(all_data) -> dict:
    return {"total": sum(v["price"] for v in all_data.values())}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let agg = &nodes[1];
    assert_eq!(agg.inputs[0].param_name, "all_data");
    assert!(agg.inputs[0].collected);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 13. Complex real-world patterns
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn spaceflights_topology() {
    use barca_core::dag::Dag;

    let src = r#"
from barca import asset

@asset()
def raw_shuttles(): return {}

@asset()
def raw_companies(): return {}

@asset()
def raw_reviews(): return {}

@asset(inputs={"raw": raw_shuttles})
def prep_shuttles(raw): return raw

@asset(inputs={"raw": raw_companies})
def prep_companies(raw): return raw

@asset(inputs={"raw": raw_reviews})
def prep_reviews(raw): return raw

@asset(inputs={"s": prep_shuttles, "c": prep_companies, "r": prep_reviews})
def master_table(s, c, r): return {}

@asset(inputs={"data": master_table})
def split(data): return {}

@asset(inputs={"data": split})
def train(data): return {}

@asset(inputs={"model": train, "data": split})
def evaluate(model, data): return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    let dag = Dag::build(&nodes).unwrap();

    assert_eq!(dag.node_count(), 10);
    // 3 raw→prep edges + 3 prep→master + master→split + split→train + train→eval + split→eval
    assert_eq!(dag.edge_count(), 10);

    // Verify topology: raw sources have no upstream, evaluate depends on train + split
    assert!(dag.upstream("test.py:raw_shuttles").is_empty());
    assert_eq!(
        dag.downstream("test.py:raw_shuttles"),
        vec!["test.py:prep_shuttles"]
    );
    let eval_upstream = dag.upstream("test.py:evaluate");
    assert_eq!(eval_upstream.len(), 2); // train + split
}

#[test]
fn all_decorators_combined() {
    // A single file with every decorator type
    let src = r#"
from barca import asset, sensor, task, sink, unsafe, Always, Manual, Schedule, partitions, collect

@sensor(freshness=Schedule("*/5 * * * *"), description="Check for new files")
def file_watcher():
    return (True, {"files": []})

@asset(freshness=Always, tags={"team": "data"})
@sink("output/raw.json", serializer="json")
def ingest() -> dict:
    return {"rows": 100}

@asset(
    inputs={"raw": ingest, "trigger": file_watcher},
    freshness=Manual,
    timeout_seconds=600,
    name="transform_v2",
    description="Main transformation pipeline",
)
def transform(raw: dict, trigger) -> dict:
    return {"transformed": raw["rows"]}

@unsafe
@asset(partitions={"region": partitions(["us", "eu", "ap"])})
def regional_export(region: str) -> dict:
    return {"region": region}

@task(inputs={"data": transform}, freshness=Always)
def notify(data):
    pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 5);

    // Sensor
    assert_eq!(nodes[0].kind, NodeKind::Sensor);
    assert_eq!(
        nodes[0].freshness,
        Freshness::Schedule(CronExpr("*/5 * * * *".into()))
    );
    assert_eq!(nodes[0].description, Some("Check for new files".into()));

    // Asset with sink
    assert_eq!(nodes[1].kind, NodeKind::Asset);
    assert_eq!(nodes[1].sinks.len(), 1);
    assert_eq!(nodes[1].tags.get("team").unwrap(), "data");

    // Asset with all metadata
    assert_eq!(nodes[2].explicit_name, Some("transform_v2".into()));
    assert_eq!(nodes[2].freshness, Freshness::Manual);
    assert_eq!(nodes[2].timeout_seconds, 600);
    assert_eq!(nodes[2].inputs.len(), 2);

    // Unsafe partitioned asset
    assert!(nodes[3].is_unsafe);
    assert!(nodes[3].partitions.contains_key("region"));

    // Task
    assert_eq!(nodes[4].kind, NodeKind::Task);
}

// ═══════════════════════════════════════════════════════════════════════════════
// 14. Negative tests / edge cases — error handling and graceful degradation
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn malformed_python_returns_err() {
    let src = "def broken(:\n    pass";
    let result = extract_nodes(src, "bad.py");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("syntax error"));
    assert!(err.contains("bad.py"));
}

#[test]
fn empty_file_returns_empty_vec() {
    let nodes = extract_nodes("", "empty.py").unwrap();
    assert!(nodes.is_empty());
}

#[test]
fn file_with_no_barca_decorators() {
    let src = r#"
def regular_function():
    return 42

class MyClass:
    pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert!(nodes.is_empty());
}

#[test]
fn inputs_not_a_dict_is_ignored() {
    let src = r#"
from barca import asset

@asset(inputs="not_a_dict")
def bad_inputs() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].inputs.is_empty()); // gracefully ignored
}

#[test]
fn timeout_not_an_int_falls_back_to_default() {
    let src = r#"
from barca import asset

@asset(timeout_seconds="not_an_int")
def bad_timeout() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].timeout_seconds, 300); // default
}

#[test]
fn empty_partition_list() {
    let src = r#"
from barca import asset, partitions

@asset(partitions={"key": partitions([])})
def empty_partitions(key: str) -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
    let spec = nodes[0].partitions.get("key").unwrap();
    match spec {
        PartitionSpec::Static { values } => assert!(values.is_empty()),
        _ => panic!("expected static partitions"),
    }
}

#[test]
fn decorator_on_class_is_ignored() {
    let src = r#"
from barca import asset

@asset()
class NotAFunction:
    pass

@asset()
def real_asset() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1); // only the function, not the class
    assert_eq!(nodes[0].function_name, "real_asset");
}

#[test]
fn aliased_import_not_detected() {
    // Documented limitation: parser matches exact decorator names.
    // Aliased imports are not supported.
    let src = r#"
from barca import asset as a

@a()
def aliased() -> dict:
    return {}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert!(nodes.is_empty()); // not detected — known limitation
}

#[test]
fn comments_and_docstrings_dont_break_parsing() {
    let src = r#"
# This is a comment
"""This is a module docstring."""

from barca import asset

# Another comment
@asset()
def my_asset() -> dict:
    """Asset docstring."""
    # inline comment
    return {"value": 1}
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Arguments a decorator does not define (#284)
// ═══════════════════════════════════════════════════════════════════════════════

/// The error text for a source that must be rejected for a decorator argument.
fn rejected(src: &str) -> String {
    match extract_nodes(src, "test.py") {
        Err(e @ barca_core::parse::ParseError::InvalidArguments { .. }) => e.to_string(),
        other => panic!("expected InvalidArguments, got {other:?}\n{src}"),
    }
}

/// `decorators` above `def node(): ...`, after the usual import and one upstream asset.
fn with_decorators(decorators: &str) -> String {
    format!(
        "from barca import *\n\n@asset()\ndef raw():\n    return 1\n\n{decorators}\ndef node():\n    return 1\n"
    )
}

/// Until 0.18.1 this test was `unknown_decorator_kwargs_are_ignored`: the node was extracted
/// and the arguments dropped without a word.
#[test]
fn unknown_decorator_kwargs_are_rejected() {
    let src = r#"
from barca import asset

@asset(unknown_param="hello", another=42)
def my_asset() -> dict:
    return {}
"#;
    let err = rejected(src);
    assert_eq!(
        err,
        "test.py:my_asset (line 4): `unknown_param` is not an argument of @asset. @asset \
         accepts: name, inputs, partitions, serializer, freshness, timeout_seconds, retries, \
         retry_backoff, description, tags, env\n\
         Remove `unknown_param`, or replace it with an argument @asset accepts. See `barca \
         docs assets`."
    );
}

#[test]
fn an_argument_from_old_documentation_is_rejected_on_each_decorator() {
    for (decorators, argument, call) in [
        ("@asset(after=raw)", "after", "@asset"),
        ("@task(when=\"always\")", "when", "@task"),
        ("@sensor(poll=5)", "poll", "@sensor"),
        (
            "@asset()\n@sink(\"out.json\", mode=\"append\")",
            "mode",
            "@sink",
        ),
        // Real arguments, on a decorator that does not take them.
        (
            "@task(partitions={\"k\": partitions([1])})",
            "partitions",
            "@task",
        ),
        ("@task(serializer=\"json\")", "serializer", "@task"),
        ("@sensor(serializer=\"json\")", "serializer", "@sensor"),
    ] {
        let err = rejected(&with_decorators(decorators));
        assert!(
            err.contains(&format!("`{argument}` is not an argument of {call}.")),
            "{decorators}: {err}"
        );
        assert!(err.starts_with("test.py:node (line "), "{err}");
        assert!(err.contains(&format!("{call} accepts: ")), "{err}");
        assert!(!err.contains("Did you mean"), "{decorators}: {err}");
    }
}

#[test]
fn a_misspelt_argument_names_the_one_it_is_close_to() {
    for (decorators, typo, meant) in [
        ("@asset(input={\"raw\": raw})", "input", "inputs"),
        (
            "@asset(partition={\"k\": partitions([1])})",
            "partition",
            "partitions",
        ),
        ("@asset(serialiser=\"pickle\")", "serialiser", "serializer"),
        ("@task(Inputs={\"raw\": raw})", "Inputs", "inputs"),
        ("@sensor(freshnes=Manual)", "freshnes", "freshness"),
        (
            "@asset()\n@sink(\"o.json\", serialiser=\"json\")",
            "serialiser",
            "serializer",
        ),
        ("@asset(retry_backof=1.0)", "retry_backof", "retry_backoff"),
        (
            "@asset(timeout_second=5)",
            "timeout_second",
            "timeout_seconds",
        ),
    ] {
        let err = rejected(&with_decorators(decorators));
        assert!(
            err.contains(&format!("`{typo}` is not an argument of @")),
            "{decorators}: {err}"
        );
        assert!(
            err.contains(&format!(" Did you mean `{meant}`? ")),
            "{decorators}: {err}"
        );
        assert!(
            err.contains(&format!("\nRename `{typo}` to `{meant}`, or remove it.")),
            "{decorators}: {err}"
        );
    }
}

#[test]
fn the_helpers_take_their_argument_by_position_only() {
    for (decorators, argument, call, usage) in [
        (
            "@asset(partitions={\"k\": partitions(values=[1, 2])})",
            "values",
            "partitions()",
            "partitions([\"a\", \"b\"])",
        ),
        (
            "@asset(partitions={\"k\": partitions_from(source=raw)})",
            "source",
            "partitions_from()",
            "partitions_from(upstream)",
        ),
        (
            "@asset(inputs={\"raw\": collect(asset_fn=raw)})",
            "asset_fn",
            "collect()",
            "collect(upstream)",
        ),
        (
            "@asset(inputs={\"raw\": asset_ref(ref_string=\"test.py:raw\")})",
            "ref_string",
            "asset_ref()",
            "asset_ref(\"file.py:name\")",
        ),
        (
            "@asset(freshness=Schedule(cron=\"0 5 * * *\"))",
            "cron",
            "Schedule()",
            "Schedule(\"0 5 * * *\")",
        ),
        (
            "@asset(freshness=Schedule(\"0 5 * * *\", timezone=\"utc\"))",
            "timezone",
            "Schedule()",
            "Schedule(\"0 5 * * *\")",
        ),
    ] {
        let err = rejected(&with_decorators(decorators));
        assert!(
            err.contains(&format!(
                "`{argument}` is not an argument of {call}. {call} takes no keyword arguments"
            )),
            "{decorators}: {err}"
        );
        assert!(
            err.contains(&format!("Pass the value by position, like `{usage}`")),
            "{decorators}: {err}"
        );
    }
    // The path of a sink is positional too; before, `@sink(path=...)` declared no sink.
    let err = rejected(&with_decorators("@asset()\n@sink(path=\"out.json\")"));
    assert!(err.contains("`path` is not an argument of @sink. @sink accepts: serializer"));
}

#[test]
fn arguments_that_cannot_be_read_from_the_source_are_rejected() {
    let err = rejected(&with_decorators("@asset(**OPTIONS)"));
    assert!(
        err.contains("@asset is called with `**` arguments. barca reads decorator arguments from the source without running it"),
        "{err}"
    );
    assert!(
        err.contains("\nWrite the arguments out, like `@asset(inputs="),
        "{err}"
    );

    // A literal dict behind `**` is not read either: no special case.
    let err = rejected(&with_decorators("@task(**{\"inputs\": {\"raw\": raw}})"));
    assert!(err.contains("@task is called with `**` arguments"), "{err}");

    let err = rejected(&with_decorators("@asset(*ARGS)"));
    assert!(err.contains("@asset is called with `*` arguments"), "{err}");

    let err = rejected(&with_decorators("@asset(\"daily\")"));
    assert!(
        err.contains(
            "@asset takes keyword arguments only, and is called with a positional argument."
        ),
        "{err}"
    );

    // `@sink("path", "json")` used to drop the serializer.
    let err = rejected(&with_decorators("@asset()\n@sink(\"out.txt\", \"json\")"));
    assert!(
        err.contains(
            "@sink takes one positional argument, and is called with 2 positional arguments."
        ),
        "{err}"
    );
}

#[test]
fn the_error_names_the_line_of_the_argument_and_the_first_problem_in_the_file() {
    let src = r#"from barca import asset, task

@asset(
    name="x",
    retries=2,
    after=None,
)
def first():
    return 1

@task(when=1)
def second():
    pass
"#;
    let err = rejected(src);
    assert!(
        err.starts_with("test.py:first (line 6): `after` is not"),
        "{err}"
    );
}

#[test]
fn every_documented_argument_is_accepted() {
    let src = r#"
from barca import asset, sensor, task, sink, partitions, partitions_from, collect, asset_ref
from barca import Always, Manual, Schedule

@asset(
    name="named",
    inputs={},
    partitions={"k": partitions(["a", "b"])},
    serializer="json",
    freshness=Always,
    timeout_seconds=10,
    retries=2,
    retry_backoff=0.5,
    description="d",
    tags={"team": "data"},
    env=["HOME"],
)
@sink("out.json", serializer="json")
def full(k):
    return 1

@asset(partitions={"k": partitions_from(full)})
def mirrored(k, full):
    return 1

@asset(inputs={"parts": collect(full), "other": asset_ref("test.py:mirrored")})
def gathered(parts, other):
    return parts

@sensor(
    name="s",
    freshness=Schedule("*/5 * * * *"),
    timeout_seconds=10,
    retries=2,
    retry_backoff=0.5,
    description="d",
    tags={"a": "b"},
    env=["HOME"],
)
def watch():
    return (True, 1)

@task(
    name="t",
    inputs={"g": gathered},
    freshness=Manual,
    timeout_seconds=10,
    retries=2,
    retry_backoff=0.5,
    description="d",
    tags={"a": "b"},
    env=["HOME"],
)
def publish(g):
    pass
"#;
    let nodes = extract_nodes(src, "test.py").unwrap();
    assert_eq!(nodes.len(), 5);
}

#[test]
fn a_decorator_that_is_not_barcas_is_not_checked() {
    // Defined in the file.
    let own = r#"
import barca

def asset(**options):
    return lambda f: f

@asset(owner="me")
def mine():
    return 1
"#;
    assert!(extract_nodes(own, "test.py").is_ok());

    // Imported from somewhere else, also under barca's name.
    let other = r#"
from other_lib import task, sink
from dagster import asset

@asset(ins={"x": 1})
@sink("p", mode="a")
def a():
    return 1

@task(when="now")
def t():
    pass
"#;
    assert!(extract_nodes(other, "test.py").is_ok());

    // A helper of the same name defined in the file, and calls that are not barca's inside
    // the arguments.
    let helpers = r#"
from barca import asset, partitions

def collect(thing, flatten=False):
    return thing

@asset()
def raw():
    return [1]

@asset(inputs={"raw": collect(raw, flatten=True)}, partitions={"k": partitions(load(kind="x"))})
def uses(raw, k):
    return raw
"#;
    assert!(extract_nodes(helpers, "test.py").is_ok());

    // A function that is no barca node: its decorators are not looked at.
    let plain = r#"
from barca import sink
import functools

@functools.lru_cache(maxsize=None)
def cached():
    return 1

@sink(path="x")
def not_a_node():
    return 1
"#;
    assert_eq!(extract_nodes(plain, "test.py").unwrap().len(), 0);
}

#[test]
fn barcas_name_is_checked_however_it_reaches_the_file() {
    for import in [
        "from barca import asset",
        "from barca import *",
        "", // no import at all: the name is still read as barca's, so it is checked
    ] {
        let src = format!("{import}\n\n@asset(after=None)\ndef a():\n    return 1\n");
        let err = rejected(&src);
        assert!(
            err.contains("`after` is not an argument of @asset"),
            "{import}: {err}"
        );
    }
}

/// `import barca as b` / `from barca import asset as a`: the parser has never read these as
/// nodes (the file then defines none), so there is nothing to check. Recorded here so that a
/// change to alias handling has to decide what the check does.
#[test]
fn aliased_decorators_are_not_nodes_and_so_not_checked() {
    let src = r#"
import barca as b
from barca import asset as a

@b.asset(bogus=1)
def one():
    return 1

@a(bogus=1)
def two():
    return 2
"#;
    assert_eq!(extract_nodes(src, "test.py").unwrap().len(), 0);
}

/// `@sensor(inputs=...)` keeps its own message, from the DAG (`dag_rejects_sensor_with_inputs`).
#[test]
fn sensor_inputs_are_left_to_the_dag_error() {
    let src = "from barca import sensor\n\n@sensor(inputs={})\ndef s():\n    return (True, 1)\n";
    assert!(extract_nodes(src, "test.py").is_ok());
}
