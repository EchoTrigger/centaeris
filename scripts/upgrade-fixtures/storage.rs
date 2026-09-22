#[cfg(test)]
mod upgrade_storage_fixture {
    #[test]
    fn export_released_storage_fixture() {
        use std::io::Write;
        let output = std::path::PathBuf::from(std::env::var("CENTAERIS_UPGRADE_FIXTURE_OUT").unwrap());
        let input = std::fs::read_to_string(output.join("session.jsonl")).unwrap();
        let mut lines = input.lines();
        let manifest = lines.next().unwrap();
        let mut wires = lines.map(|line| serde_json::from_str(line).unwrap()).collect::<Vec<_>>();
        let path = output.join("session-1.jsonl");
        std::fs::write(&path, format!("{manifest}\n")).unwrap();
        super::observation_cas::compact_and_install_wires(&path, "session-1", &mut wires).unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        for wire in wires {
            serde_json::to_writer(&mut file, &wire).unwrap();
            writeln!(file).unwrap();
        }
    }
}
