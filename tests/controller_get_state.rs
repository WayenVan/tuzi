use std::{process::Stdio, time::{Duration, SystemTime, UNIX_EPOCH}};

use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines}, process::{ChildStdout, Command}, time::timeout};
use tuzi::{dds::{self, Body}, session_state::{SessionState, TabState}};

async fn read_json(lines: &mut Lines<BufReader<ChildStdout>>) -> serde_json::Value {
	let line = timeout(Duration::from_secs(3), lines.next_line()).await.unwrap().unwrap().unwrap();
	serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn controller_get_state_waits_for_the_target_peers_snapshot() {
	let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
	let root = std::env::temp_dir().join(format!("tuzi-controller-get-state-{}-{nonce}", std::process::id()));
	std::fs::create_dir_all(&root).unwrap();
	let socket = root.join("dds.sock");
	let mut process = Command::new(env!("CARGO_BIN_EXE_tu"))
		.args(["dds", "--socket"]).arg(&socket).arg("controller")
		.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
		.kill_on_drop(true).spawn().unwrap();
	let mut input = process.stdin.take().unwrap();
	let mut output = BufReader::new(process.stdout.take().unwrap()).lines();
	let ready = read_json(&mut output).await;
	assert_eq!(ready["event"], "controller-ready");
	let controller_id = ready["peer_id"].as_u64().unwrap();

	input.write_all(b"{\"request_id\":1,\"op\":\"register\",\"token\":\"test-token\"}\n").await.unwrap();
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "request_id": 1, "ok": true }));
	let (peer, mut inbox) = dds::Client::connect(&socket, vec!["get-state".into(), "get-tabs".into(), "switch-tab".into(), "update-tab".into()]).await.unwrap();
	peer.publish_to(controller_id, Body::Attach { token: "test-token".into() });
	let attached = read_json(&mut output).await;
	assert_eq!(attached["event"], "tuzi-ready");
	assert_eq!(attached["peer_id"], peer.id());

	input.write_all(format!("{}\n", serde_json::json!({ "request_id": 2, "op": "get-state", "peer_id": peer.id() })).as_bytes()).await.unwrap();
	let query_id = loop {
		let payload = timeout(Duration::from_secs(3), inbox.recv()).await.unwrap().unwrap();
		if let Body::GetState { query_id } = payload.body {
			assert_eq!(payload.receiver, peer.id());
			break query_id;
		}
	};
	input.write_all(b"{\"request_id\":2,\"op\":\"ping\"}\n").await.unwrap();
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "ok": false, "error": "request_id is already pending" }));
	let state = SessionState {
		version: 1,
		active_tab: 0,
		tabs: vec![TabState { cwd: root.clone(), cursor: None, selection: Vec::new(), expanded: Vec::new() }],
	};
	peer.publish_to(controller_id, Body::State { query_id, state: state.clone() });
	let response = read_json(&mut output).await;
	assert_eq!(response, serde_json::json!({ "request_id": 2, "ok": true, "state": state }));

	input.write_all(format!("{}\n", serde_json::json!({ "request_id": 3, "op": "get-state", "peer_id": peer.id() })).as_bytes()).await.unwrap();
	let query_id = loop {
		let payload = timeout(Duration::from_secs(3), inbox.recv()).await.unwrap().unwrap();
		if let Body::GetState { query_id } = payload.body { break query_id }
	};
	peer.publish_to(controller_id, Body::StateError { query_id, error: "snapshot unavailable".into() });
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "request_id": 3, "ok": false, "error": "snapshot unavailable" }));

	input.write_all(format!("{}\n", serde_json::json!({ "request_id": 4, "op": "get-tabs", "peer_id": peer.id() })).as_bytes()).await.unwrap();
	let query_id = loop {
		let payload = timeout(Duration::from_secs(3), inbox.recv()).await.unwrap().unwrap();
		if let Body::GetTabs { query_id } = payload.body { break query_id }
	};
	let tabs = vec![dds::TabInfo { id: 8, cwd: root.clone() }, dds::TabInfo { id: 13, cwd: root.clone() }];
	peer.publish_to(controller_id, Body::Tabs { query_id, active_tab_id: 13, tabs: tabs.clone() });
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "request_id": 4, "ok": true, "active_tab_id": 13, "tabs": tabs }));

	input.write_all(format!("{}\n", serde_json::json!({ "request_id": 5, "op": "switch-tab", "peer_id": peer.id(), "tab_id": 8 })).as_bytes()).await.unwrap();
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "request_id": 5, "ok": true, "status": "queued" }));
	let switched = loop {
		let payload = timeout(Duration::from_secs(3), inbox.recv()).await.unwrap().unwrap();
		if payload.body.kind() == "switch-tab" { break payload }
	};
	assert_eq!(switched.receiver, peer.id());
	assert_eq!(switched.body, Body::Custom { kind: "switch-tab".into(), data: serde_json::json!({ "tab_id": 8 }) });

	input.write_all(format!("{}\n", serde_json::json!({ "request_id": 6, "op": "update-tab", "peer_id": peer.id(), "update": { "path": root } })).as_bytes()).await.unwrap();
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "request_id": 6, "ok": true, "status": "queued" }));
	let updated = loop {
		let payload = timeout(Duration::from_secs(3), inbox.recv()).await.unwrap().unwrap();
		if payload.body.kind() == "update-tab" { break payload }
	};
	assert_eq!(updated.receiver, peer.id());

	peer.publish_to(controller_id, Body::SessionEnd { state: state.clone() });
	assert_eq!(read_json(&mut output).await, serde_json::json!({ "event": "tuzi-exit", "peer_id": peer.id(), "state": state }));

	drop(input);
	timeout(Duration::from_secs(3), process.wait()).await.unwrap().unwrap();
	drop(peer);
	std::fs::remove_dir_all(root).unwrap();
}
