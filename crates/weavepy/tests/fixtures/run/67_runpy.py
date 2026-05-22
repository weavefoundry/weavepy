import os
import sys
import runpy

here = os.path.dirname(os.path.abspath(__file__))
if here not in sys.path:
    sys.path.insert(0, here)

ns = runpy.run_module("_module_to_run", run_name="__main__")
print("namespace value:", ns["value"])
print("namespace doubled:", ns["doubled"])

ns2 = runpy.run_path(os.path.join(here, "_module_to_run.py"), run_name="not_main")
print("value via run_path:", ns2["value"])
