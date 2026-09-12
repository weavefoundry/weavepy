# Module-spec startup imports

Unimplemented lead, not a measured optimization. The startup allocation trace
contains ensure_module_spec in stacks totaling about 3.9 MB, overlapping other
categories. Most of that includes importlib bootstrap machinery that remains
necessary; it is not an avoidable-allocation estimate.

_weave_spec.make_spec_and_loader imports cache_from_source from importlib.util
for source modules. The bundled importlib_util.py directly re-exports that
function from importlib._bootstrap_external, which is already loaded by
importlib.machinery. A direct internal import could avoid bringing in util's
additional definitions and importlib._abc/types dependencies solely to compute
spec.cached. First diagnose the actual startup import inventory/dependency tree.
Other paths may already import util, making the change irrelevant.

CPython's ModuleSpec cached logic uses bootstrap internals. A direct import can
change how patches to the public util alias affect this private helper. Validate
module specs/loaders/cache paths, namespace search-path identity, patched
bootstrap functions, fresh importlib copies, circular/bootstrap imports, and
source/extension/frozen module taxonomy. Do not bypass real spec construction or
assume all 3.9 MB can disappear. No runtime changes during ownership validation.
