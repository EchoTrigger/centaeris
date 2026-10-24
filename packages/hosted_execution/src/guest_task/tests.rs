use super::*;
use serde_json::json;

pub(super) fn bundle() -> Value {
    json!({
        "ociVersion":"1.2.0",
        "root":{"path":"rootfs","readonly":false},
        "process":{
            "terminal":false,"user":{"uid":0,"gid":0,"additionalGids":[]},
            "args":["/keeper"],"cwd":"/app",
            "env":["PATH=/usr/sbin:/usr/bin:/sbin:/bin","HOME=/root","LANG=C.UTF-8","CENTAERIS_WORKSPACE_ROOT=/","CENTAERIS_WORKSPACE_USER=0:0"],
            "noNewPrivileges":true,
            "capabilities":{"bounding":["CAP_CHOWN","CAP_NET_BIND_SERVICE"],"effective":["CAP_CHOWN","CAP_NET_BIND_SERVICE"],"permitted":["CAP_CHOWN","CAP_NET_BIND_SERVICE"],"inheritable":[],"ambient":[]}
        },
        "mounts":[
            {"destination":"/proc","type":"proc","source":"proc","options":["nosuid","noexec","nodev"]},
            {"destination":"/dev","type":"tmpfs","source":"tmpfs","options":["nosuid"]},
            {"destination":"/dev/pts","type":"devpts","source":"devpts","options":["nosuid","noexec"]},
            {"destination":"/dev/shm","type":"tmpfs","source":"shm","options":["nosuid","nodev"]},
            {"destination":"/sys","type":"sysfs","source":"sysfs","options":["ro","nosuid","noexec","nodev"]},
            {"destination":"/sys/fs/cgroup","type":"cgroup","source":"cgroup","options":["ro","nosuid","noexec","nodev"]}
        ],
        "linux":{
            "namespaces":[{"type":"pid"},{"type":"mount"},{"type":"network"},{"type":"ipc"},{"type":"uts"},{"type":"user"},{"type":"cgroup"}],
            "uidMappings":[{"containerID":0,"hostID":100000,"size":65536}],
            "gidMappings":[{"containerID":0,"hostID":100000,"size":65536}],
            "cgroupsPath":"/centaeris-guest/workload",
            "resources":{"devices":[{"allow":false,"access":"rwm"}]},
            "seccomp":{"defaultAction":"SCMP_ACT_ERRNO","syscalls":[{"names":["read","write","exit"],"action":"SCMP_ACT_ALLOW"}]}
        }
    })
}

pub(super) fn manifest(spec: &Value) -> GuestWorkloadManifest {
    GuestWorkloadManifest {
        schema: GUEST_WORKLOAD_SCHEMA.into(),
        image_digest: format!("sha256:{}", "a".repeat(64)),
        spec_digest: spec_digest(spec).unwrap(),
        image_process: GuestImageProcess {
            user: GuestImageUser {
                uid: 0,
                gid: 0,
                additional_gids: vec![],
            },
            cwd: "/app".into(),
            env: vec![
                "PATH=/usr/sbin:/usr/bin:/sbin:/bin".into(),
                "HOME=/root".into(),
                "LANG=C.UTF-8".into(),
            ],
        },
        workspace_root: "/".into(),
    }
}

#[test]
fn original_root_workdir_environment_and_low_port_capability_are_preserved() {
    let spec = bundle();
    let frozen = manifest(&spec);
    frozen.validate_bundle(&spec).unwrap();
    let process = crate::host_supervisor::task_process(
        &spec["process"],
        "/usr/bin/apt-get",
        &["update".into()],
        &frozen.image_process.cwd,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(
        process["user"],
        json!({"uid":0,"gid":0,"additionalGids":[]})
    );
    assert_eq!(process["cwd"], "/app");
    assert_eq!(process["env"], spec["process"]["env"]);
    assert_eq!(process["capabilities"], spec["process"]["capabilities"]);
    assert_eq!(
        crate::host_supervisor::workspace_context(&process).unwrap(),
        ("/".into(), "0:0".into())
    );
}

#[test]
fn original_non_root_image_identity_is_not_replaced_with_a_default_user() {
    let mut spec = bundle();
    spec["process"]["user"] = json!({"uid":1042,"gid":1043,"additionalGids":[5]});
    spec["process"]["cwd"] = json!("/srv/project");
    spec["process"]["env"][4] = json!("CENTAERIS_WORKSPACE_USER=1042:1043");
    let mut frozen = manifest(&spec);
    frozen.image_process.user = GuestImageUser {
        uid: 1042,
        gid: 1043,
        additional_gids: vec![5],
    };
    frozen.image_process.cwd = "/srv/project".into();
    frozen.validate_bundle(&spec).unwrap();
}

#[test]
fn manifest_is_strict_and_cannot_select_host_control_paths() {
    let spec = bundle();
    let value = serde_json::to_value(manifest(&spec)).unwrap();
    for field in [
        "bundle",
        "runtimePath",
        "socketPath",
        "controlRoot",
        "helperPath",
    ] {
        let mut altered = value.clone();
        altered[field] = json!("/tmp/attacker");
        assert!(GuestWorkloadManifest::parse(&serde_json::to_vec(&altered).unwrap()).is_err());
    }
    let mut altered = value;
    altered["imageProcess"]["user"]["username"] = json!("root");
    assert!(GuestWorkloadManifest::parse(&serde_json::to_vec(&altered).unwrap()).is_err());
    let mut frozen = manifest(&spec);
    frozen.schema = "environment.guest.workload.v0".into();
    assert_eq!(
        frozen.validate().unwrap_err(),
        "guest_manifest_schema_unsupported"
    );
}

#[test]
fn manifest_rejects_invalid_digests_paths_and_reserved_image_environment() {
    let spec = bundle();
    for digest in [
        "",
        "sha256:abc",
        "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "image:latest",
    ] {
        let mut frozen = manifest(&spec);
        frozen.image_digest = digest.into();
        assert!(frozen.validate().is_err(), "invalid digest {digest}");
    }
    for path in [
        "relative",
        "/app/../etc",
        "/mnt//data",
        "/run/centaeris",
        "/app\n",
    ] {
        let mut frozen = manifest(&spec);
        frozen.workspace_root = path.into();
        assert!(frozen.validate().is_err(), "invalid workspace {path:?}");
    }
    let mut frozen = manifest(&spec);
    frozen
        .image_process
        .env
        .push("CENTAERIS_WORKSPACE_ROOT=/attacker".into());
    assert!(frozen.validate().is_err());
}

#[test]
fn frozen_original_process_and_exact_bundle_digest_cannot_drift() {
    let spec = bundle();
    let frozen = manifest(&spec);
    for (field, replacement) in [
        ("cwd", json!("/mnt/data")),
        ("user", json!({"uid":10001,"gid":10001,"additionalGids":[]})),
        ("env", json!(["PATH=/bin"])),
    ] {
        let mut changed = spec.clone();
        changed["process"][field] = replacement;
        assert!(frozen.validate_bundle(&changed).is_err());
        let mut resigned = frozen.clone();
        resigned.spec_digest = spec_digest(&changed).unwrap();
        assert!(resigned.validate_bundle(&changed).is_err());
    }
    let mut changed = spec.clone();
    changed["hostname"] = json!("different-target");
    assert!(frozen.validate_bundle(&changed).is_err());
}

#[test]
fn private_namespaces_and_nonzero_outer_user_mapping_are_required() {
    let spec = bundle();
    for index in 0..7 {
        let mut changed = spec.clone();
        changed["linux"]["namespaces"]
            .as_array_mut()
            .unwrap()
            .remove(index);
        assert!(manifest(&changed).validate_bundle(&changed).is_err());
        let mut changed = spec.clone();
        changed["linux"]["namespaces"][index]["path"] = json!("/proc/1/ns/user");
        assert!(manifest(&changed).validate_bundle(&changed).is_err());
    }
    for field in ["uidMappings", "gidMappings"] {
        let mut changed = spec.clone();
        changed["linux"][field][0]["hostID"] = json!(0);
        assert!(manifest(&changed).validate_bundle(&changed).is_err());
        changed["linux"][field][0]["hostID"] = json!(100000);
        changed["linux"][field][0]["size"] = json!(0);
        assert!(manifest(&changed).validate_bundle(&changed).is_err());
    }
}

#[test]
fn unsafe_control_mounts_hooks_devices_and_security_profiles_fail_closed() {
    let spec = bundle();
    let mut all_types = spec.clone();
    all_types["linux"]["resources"]["devices"][0]["type"] = json!("a");
    manifest(&all_types).validate_bundle(&all_types).unwrap();
    let mut cases = vec![];
    for device_type in ["b", "c"] {
        let mut changed = spec.clone();
        changed["linux"]["resources"]["devices"][0]["type"] = json!(device_type);
        cases.push(changed);
    }
    let mut changed = spec.clone();
    changed["mounts"].as_array_mut().unwrap().push(json!({"source":"/run/centaeris-guest","destination":"/control","type":"bind","options":["bind","ro"]}));
    cases.push(changed);
    let mut changed = spec.clone();
    changed["hooks"] = json!({"prestart":[{"path":"/bin/sh"}]});
    cases.push(changed);
    let mut changed = spec.clone();
    changed["root"]["path"] = json!("/");
    cases.push(changed);
    let mut changed = spec.clone();
    changed["process"]["noNewPrivileges"] = json!(false);
    cases.push(changed);
    let mut changed = spec.clone();
    changed["linux"]["seccomp"]["defaultAction"] = json!("SCMP_ACT_ALLOW");
    cases.push(changed);
    let mut changed = spec.clone();
    changed["linux"]["devices"] = json!([{"path":"/dev/mem","type":"c","major":1,"minor":1}]);
    cases.push(changed);
    let mut changed = spec.clone();
    changed["process"]["capabilities"]["effective"]
        .as_array_mut()
        .unwrap()
        .push(json!("CAP_SYS_ADMIN"));
    cases.push(changed);
    let mut changed = spec.clone();
    changed["mounts"][5]["options"] = json!(["rw"]);
    cases.push(changed);
    for changed in cases {
        assert!(manifest(&changed).validate_bundle(&changed).is_err());
    }
}
