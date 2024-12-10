import json
import pathlib
import sys
import os

out = pathlib.Path(os.environ["out"])
out.mkdir()

for name, data in json.loads(sys.args[1]):
    (out / f"{name}.json").write_text(json.dumps(data))
