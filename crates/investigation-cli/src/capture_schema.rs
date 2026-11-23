//! `schema capture` and `schema capture.*`: discovery for producers.
//!
//! The request and receipt schemas are generated from the Rust types the
//! recorder uses, and every limit and version comes from the code that enforces
//! it. The examples are the files under `fixtures/capture/v1`, which a test runs
//! through the built CLI.

use investigation_recording::{
    ArtifactRef, BeginCapture, BeginReceipt, CaptureCapabilities, CaptureEvent, FinalizeCapture,
    FinalizeReceipt, ItemReceipt, RecoverScope, RecoveryState, StableArtifact,
};
use schemars::schema_for;
use serde_json::{Value, json};

use crate::Output;

/// Wire schema names this module serves, overview first.
pub(crate) const NAMES: &[&str] = &[
    "capture",
    "capture.begin",
    "capture.begin.receipt",
    "capture.append",
    "capture.append.receipt",
    "capture.attach",
    "capture.attach.receipt",
    "capture.finalize",
    "capture.finalize.receipt",
    "capture.recover",
    "capture.recover.receipt",
];

struct Step {
    operation: &'static str,
    command: &'static str,
    purpose: &'static str,
    example: &'static str,
}

const STEPS: [Step; 5] = [
    Step {
        operation: "capture.begin",
        command: "capture begin --input begin.json",
        purpose: "Declare one Run before acting and allocate staging. Returns the handle, limits and negotiated protocol.",
        example: include_str!("../../../fixtures/capture/v1/begin.json"),
    },
    Step {
        operation: "capture.append",
        command: "capture append --input append.json",
        purpose: "Stage one bounded lifecycle or collection fact at the handle's next position.",
        example: include_str!("../../../fixtures/capture/v1/append.json"),
    },
    Step {
        operation: "capture.attach",
        command: "capture attach --input attach.json",
        purpose: "Copy one closed file beneath an import root into staging and verify it, or record it as missing.",
        example: include_str!("../../../fixtures/capture/v1/attach.json"),
    },
    Step {
        operation: "capture.finalize",
        command: "capture finalize --input finalize.json",
        purpose: "Seal the handle and commit the manifest, execution state and completeness in one transaction.",
        example: include_str!("../../../fixtures/capture/v1/finalize.json"),
    },
    Step {
        operation: "capture.recover",
        command: "capture recover --handle HANDLE",
        purpose: "Read what was durably accepted and the exact commands that continue the recording. Never reruns anything.",
        example: include_str!("../../../fixtures/capture/v1/recover.json"),
    },
];

/// The JSON Schema for one name in [`NAMES`].
pub(crate) fn schema(name: &str) -> Option<Value> {
    let schema = match name {
        "capture" => schema_for!(CaptureCapabilities),
        "capture.begin" => schema_for!(BeginCapture),
        "capture.begin.receipt" => schema_for!(BeginReceipt),
        "capture.append" => schema_for!(CaptureEvent),
        "capture.append.receipt" => schema_for!(ItemReceipt),
        "capture.attach" => schema_for!(StableArtifact),
        "capture.attach.receipt" => schema_for!(ArtifactRef),
        "capture.finalize" => schema_for!(FinalizeCapture),
        "capture.finalize.receipt" => schema_for!(FinalizeReceipt),
        "capture.recover" => schema_for!(RecoverScope),
        "capture.recover.receipt" => schema_for!(RecoveryState),
        _ => return None,
    };
    Some(serde_json::to_value(schema).expect("capture schema serializes"))
}

fn example(step: &Step) -> Value {
    serde_json::from_str(step.example).expect("capture fixtures are JSON")
}

/// The discovery document: overview for `capture`, one operation otherwise.
pub(crate) fn describe(name: &str) -> Output {
    let schema = schema(name).expect("name comes from NAMES");
    if name == "capture" {
        let capabilities =
            serde_json::to_value(CaptureCapabilities::current()).expect("capabilities serialize");
        let lifecycle = json!([
            "begin",
            "append | attach (positions 1, 2, 3, ...)",
            "finalize",
            "recover (read only, any time)"
        ]);
        let operations: Vec<_> = STEPS
            .iter()
            .map(|step| {
                json!({
                    "operation": step.operation,
                    "command": format!("investigation --workspace PATH {} --format json", step.command),
                    "request_schema": step.operation,
                    "receipt_schema": format!("{}.receipt", step.operation),
                    "purpose": step.purpose,
                })
            })
            .collect();
        let negotiation = "A begin request may carry protocol, the version the producer requires. A version not listed in capabilities.protocols is refused at /protocol with the supported list in error.expected, and nothing is declared.";
        let mut text = vec![
            "capture  Record an external producer's run: declare, stage facts and files, finalize, recover.".to_owned(),
            "Lifecycle: begin -> append / attach -> finalize; recover reads at any time.".to_owned(),
        ];
        for step in &STEPS {
            text.push(format!("{}  {}", step.operation, step.purpose));
        }
        let limits = &capabilities["limits"];
        text.push(format!(
            "Protocols: {}  (default {})",
            capabilities["protocols"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
            capabilities["default_protocol"]
                .as_str()
                .unwrap_or_default(),
        ));
        text.push(format!(
            "Limits per handle: {} bytes per event, {} bytes per attachment, {} positions",
            limits["max_event_bytes"], limits["max_attachment_bytes"], limits["max_items"],
        ));
        text.push(format!("Negotiation: {negotiation}"));
        text.push("Examples: fixtures/capture/v1. Inspect one with investigation schema capture.begin --format json.".to_owned());
        let value = json!({
            "name": name,
            "purpose": "Discover the capture protocol: versions, limits, operations and examples.",
            "capabilities": capabilities,
            "lifecycle": lifecycle,
            "operations": operations,
            "negotiation": negotiation,
            "schema": schema,
        });
        return Output::success("schema", value, text.join("\n"));
    }

    let (request, receipt) = name
        .strip_suffix(".receipt")
        .map_or((name, false), |request| (request, true));
    let step = STEPS
        .iter()
        .find(|step| step.operation == request)
        .expect("name comes from NAMES");
    let purpose = if receipt {
        format!("The receipt that {request} returns.")
    } else {
        step.purpose.to_owned()
    };
    let mut text = format!("{name}  {purpose}\n");
    let mut value = json!({"name": name, "purpose": purpose, "schema": schema});
    if !receipt {
        let example = example(step);
        text.push_str(&format!(
            "Command: investigation --workspace PATH {} --format json\nReceipt schema: {request}.receipt\nExample (fixtures/capture/v1; use the handle from the begin receipt)\n{}\n",
            step.command,
            serde_json::to_string_pretty(&example).expect("JSON serializes"),
        ));
        value["example"] = example;
        value["receipt_schema"] = json!(format!("{request}.receipt"));
    }
    text.push_str(&serde_json::to_string_pretty(&schema).expect("JSON serializes"));
    Output::success("schema", value, text)
}
