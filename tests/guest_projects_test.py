"""Exercise guest project publication with real mounts in a disposable namespace."""
import json
from pathlib import Path
import subprocess


def main():
    root = Path(__file__).resolve().parent.parent
    build = subprocess.run(
        ["node", "scripts/test-backend.mjs", "--test", "guest_projects", "--no-run", "--message-format=json"],
        cwd=root, check=True, text=True, stdout=subprocess.PIPE,
    )
    executable = next(
        row["executable"] for line in build.stdout.splitlines()
        if (row := json.loads(line)).get("reason") == "compiler-artifact"
        and row.get("target", {}).get("name") == "guest_projects" and row.get("executable")
    )
    subprocess.run(
        ["sudo", "-n", "unshare", "--mount", "--propagation", "private", "sh", "-c",
         'mount -t tmpfs tmpfs /var/lib && export LEO_PROJECT_MOUNT_TEST=1 && exec "$1" --ignored --nocapture --test-threads=1',
         "guest-project-test", executable],
        cwd=root, check=True,
    )


if __name__ == "__main__":
    main()
