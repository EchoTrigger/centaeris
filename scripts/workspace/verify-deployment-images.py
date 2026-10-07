"""Verify that Runtime resolves the images produced by this Compose build."""
import json
import subprocess
import sys


def inspect_image(reference):
    output = subprocess.run(
        ["docker", "image", "inspect", reference], capture_output=True, text=True, check=True,
    )
    return json.loads(output.stdout)[0]


def verify_images(config, inspect=inspect_image):
    services = config["services"]
    runtime = services["runtime"]["environment"]
    for service, host, variable in (("document-processor", "material-worker", "MATERIAL_PROCESSOR_IMAGE"),
                                    ("workspace-general", "runtime", "WORKSPACE_GENERAL_IMAGE")):
        runtime = services[host]["environment"]
        built = inspect(services[service]["image"])
        referenced = inspect(runtime[variable])
        if built["Id"] != referenced["Id"]:
            raise ValueError(f"{service}: Runtime references a different image than the Compose build")
        if service == "document-processor":
            environment = dict(item.split("=", 1) for item in built["Config"]["Env"])
            if environment.get("CENTAERIS_PROCESSOR_DEVICE") != runtime["MATERIAL_PROCESSOR_DEVICE"]:
                raise ValueError("document-processor: built device differs from Runtime device")


if __name__ == "__main__":
    verify_images(json.load(sys.stdin))
    print("Runtime image identities and processor device match the Compose build.")
