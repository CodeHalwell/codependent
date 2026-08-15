//! Integration tests for provider VCR cassettes (Adoption 12 A3).

use std::sync::{Arc, Mutex};

use codypendent_protocol::ModelId;
use codypendent_runtime::agent::{DeltaSink, ModelDriver, ModelStep, ScriptedDriver};
use codypendent_runtime::vcr::{Cassette, CassetteDriver, RecordingDriver};
use tempfile::NamedTempFile;

struct BufferSink(Vec<String>);
impl DeltaSink for BufferSink {
    fn on_text(&mut self, text: &str) {
        self.0.push(text.to_string());
    }
}

#[tokio::test]
async fn vcr_file_round_trip_recording_and_playback() {
    let temp_file = NamedTempFile::new().expect("create temp file");
    let cassette_path = temp_file.path().to_path_buf();
    let model_id = ModelId("claude-3-5-sonnet".to_string());

    let cassette_arc = Arc::new(Mutex::new(Cassette::new(model_id.clone())));
    let scripted = ScriptedDriver::new(vec![
        ModelStep::Say("Step 1: Analyzing issue".to_string()),
        ModelStep::Finish {
            summary: "Resolved successfully".to_string(),
        },
    ])
    .with_model(model_id.clone());

    let recording_driver = RecordingDriver::new(scripted, Arc::clone(&cassette_arc));

    // Execute first step
    let mut sink = BufferSink(Vec::new());
    let step1 = recording_driver
        .next_step(&[], &[], &mut sink)
        .await
        .expect("step 1");
    assert_eq!(
        step1.step,
        ModelStep::Say("Step 1: Analyzing issue".to_string())
    );

    // Save cassette to file
    {
        let mut cassette = cassette_arc.lock().unwrap();
        cassette
            .save_to_file(&cassette_path)
            .expect("save cassette");
    }

    // Load cassette from file and play back
    let loaded_cassette = Cassette::load_from_file(&cassette_path).expect("load cassette");
    assert_eq!(loaded_cassette.interactions.len(), 1);

    let playback_driver = CassetteDriver::new(loaded_cassette);
    let mut play_sink = BufferSink(Vec::new());
    let play_step = playback_driver
        .next_step(&[], &[], &mut play_sink)
        .await
        .expect("playback step 1");
    assert_eq!(
        play_step.step,
        ModelStep::Say("Step 1: Analyzing issue".to_string())
    );
}

#[tokio::test]
async fn vcr_detects_exhausted_interactions() {
    let model_id = ModelId("claude-3-5-sonnet".to_string());
    let empty_cassette = Cassette::new(model_id);
    let playback_driver = CassetteDriver::new(empty_cassette);

    let mut sink = BufferSink(Vec::new());
    let result = playback_driver.next_step(&[], &[], &mut sink).await;
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("Cassette exhausted"));
}
