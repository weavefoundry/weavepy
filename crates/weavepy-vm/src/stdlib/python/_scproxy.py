"""WeavePy ``_scproxy`` shim.

CPython's ``_scproxy`` is a macOS C extension that reads the system proxy
configuration through the SystemConfiguration framework. WeavePy does not
bind that framework, so this shim reports "no system proxy configured" — the
same result ``urllib.request.getproxies_macosx_sysconf()`` produces when the
SystemConfiguration store holds no proxy entries. Environment-variable
proxies (``getproxies_environment``) continue to work and take precedence,
matching CPython's behaviour.
"""


def _get_proxy_settings():
    # Shape mirrors the real extension: a global on/off plus a bypass list.
    return {"exclude_simple": False, "exceptions": []}


def _get_proxies():
    # scheme -> proxy URL; empty means "no system proxies".
    return {}
