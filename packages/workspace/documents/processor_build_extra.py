"""Map the exact processor device identity to its installation extra."""
import sys


def build_extra(device: str) -> str:
    if device == "cpu":
        return "cpu"
    if device == "gpu:0":
        return "gpu"
    raise ValueError("processor device must be exactly cpu or gpu:0")


if __name__ == "__main__":
    print(build_extra(sys.argv[1]))
