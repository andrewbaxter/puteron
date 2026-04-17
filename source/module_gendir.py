import json
import pathlib
import sys
import os
import subprocess

out = pathlib.Path(os.environ["out"])
out.mkdir()

for name, data in json.loads(pathlib.Path(sys.argv[1]).read_text()).items():
    (out / f"{name}.json").write_text(json.dumps(data))

subprocess.check_call([sys.argv[2], "validate-task-configs", out])