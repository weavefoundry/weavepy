"""``_pluggy`` — minimal pluggy-shape plugin host for ``_pytest``.

Implements the slice of pluggy that the bundled ``_pytest`` needs:

* ``HookspecMarker(project_name)`` / ``HookimplMarker(project_name)`` —
  decorator factories used to declare hook specs and implementations.
* ``PluginManager(project_name)`` — registry that gathers plugins,
  routes hook calls, and supports `register`, `unregister`,
  `set_blocked`, `is_blocked`, `hook` attribute access.
* ``HookCaller`` — runs all registered impls and returns either the
  list of results or the first non-None result, depending on
  `firstresult`.

The shape mirrors pluggy ≥1.4 closely enough that user code that
imports ``import pluggy`` gets a working object graph.
"""

import inspect


__all__ = [
    'HookspecMarker', 'HookimplMarker', 'PluginManager',
    'PluginValidationError', 'HookCallError',
]


class PluginValidationError(Exception):
    pass


class HookCallError(Exception):
    pass


class _MarkerBase:
    def __init__(self, project_name: str, attr: str):
        self.project_name = project_name
        self._attr = attr

    def __call__(self, function=None, **kwargs):
        if function is None:
            def deco(fn):
                setattr(fn, self._attr, dict(kwargs))
                return fn
            return deco
        if callable(function):
            setattr(function, self._attr, {})
            return function
        # If called with kwargs but no function, return the decorator.
        def deco(fn):
            setattr(fn, self._attr, dict(kwargs))
            return fn
        return deco


class HookspecMarker(_MarkerBase):
    def __init__(self, project_name: str):
        super().__init__(project_name, '__pluggy_hookspec__')


class HookimplMarker(_MarkerBase):
    def __init__(self, project_name: str):
        super().__init__(project_name, '__pluggy_hookimpl__')


class HookCaller:
    """One hook bucket: routes calls to every registered implementation."""

    __slots__ = ('name', 'spec_params', 'firstresult', 'historic', 'impls')

    def __init__(self, name: str, spec_params: tuple, firstresult: bool = False,
                 historic: bool = False):
        self.name = name
        self.spec_params = spec_params
        self.firstresult = firstresult
        self.historic = historic
        self.impls = []

    def add(self, fn, kwargs):
        sig = inspect.signature(fn)
        params = tuple(sig.parameters.keys())
        self.impls.append({
            'fn': fn,
            'params': params,
            'tryfirst': bool(kwargs.get('tryfirst')),
            'trylast': bool(kwargs.get('trylast')),
            'hookwrapper': bool(kwargs.get('hookwrapper')),
            'wrapper': bool(kwargs.get('wrapper')),
            'specname': kwargs.get('specname'),
        })
        self.impls.sort(key=lambda i: (-1 if i['tryfirst'] else (1 if i['trylast'] else 0)))

    def __call__(self, **kwargs):
        results = []
        for impl in self.impls:
            args = [kwargs[p] for p in impl['params'] if p in kwargs]
            kw = {k: v for k, v in kwargs.items() if k in impl['params']}
            try:
                rv = impl['fn'](**kw)
            except Exception as exc:
                if self.firstresult:
                    raise
                # Match pluggy: errors are propagated unless caller handles them.
                raise
            if self.firstresult and rv is not None:
                return rv
            results.append(rv)
        if self.firstresult:
            return None
        return results

    def call_extra(self, methods, kwargs):
        """Temporarily extend the impl list with extra functions."""
        original = list(self.impls)
        try:
            for fn in methods:
                self.impls.append({
                    'fn': fn,
                    'params': tuple(inspect.signature(fn).parameters.keys()),
                    'tryfirst': False, 'trylast': False,
                    'hookwrapper': False, 'wrapper': False,
                    'specname': None,
                })
            return self(**kwargs)
        finally:
            self.impls = original

    def get_hookimpls(self):
        return list(self.impls)


class _HookRelay:
    """Attribute accessor for hook callers (mirrors pluggy.PluginManager.hook)."""

    def __init__(self):
        self._hooks: Dict[str, HookCaller] = {}

    def __getattr__(self, name: str) -> HookCaller:
        if name.startswith('_'):
            raise AttributeError(name)
        if name not in self._hooks:
            self._hooks[name] = HookCaller(name, ())
        return self._hooks[name]

    def __contains__(self, name: str) -> bool:
        return name in self._hooks

    def _add(self, name: str, caller: HookCaller):
        self._hooks[name] = caller


class PluginManager:
    """Plugin registry + hook router."""

    def __init__(self, project_name: str):
        self.project_name = project_name
        self.hook = _HookRelay()
        self._plugins: Dict[str, Any] = {}
        self._blocked: set = set()
        self._spec_attr = '__pluggy_hookspec__'
        self._impl_attr = '__pluggy_hookimpl__'

    # ---------- spec registration

    def add_hookspecs(self, module_or_class) -> None:
        for name in dir(module_or_class):
            obj = getattr(module_or_class, name)
            spec = getattr(obj, self._spec_attr, None)
            if spec is None:
                continue
            sig = inspect.signature(obj)
            caller = HookCaller(
                name,
                tuple(sig.parameters.keys()),
                firstresult=bool(spec.get('firstresult')),
                historic=bool(spec.get('historic')),
            )
            self.hook._add(name, caller)

    # ---------- plugin registration

    def register(self, plugin, name: str = None) -> str:
        if plugin is None:
            return None
        pname = name or _pluginname(plugin)
        if pname in self._plugins:
            return pname
        if pname in self._blocked:
            return None
        self._plugins[pname] = plugin
        for attr in dir(plugin):
            obj = getattr(plugin, attr, None)
            if obj is None:
                continue
            impl = getattr(obj, self._impl_attr, None)
            if impl is None:
                continue
            specname = impl.get('specname') or attr
            caller = self.hook._hooks.get(specname)
            if caller is None:
                caller = HookCaller(specname, ())
                self.hook._add(specname, caller)
            caller.add(obj, impl)
        return pname

    def unregister(self, plugin=None, name: str = None):
        if name is None:
            name = _pluginname(plugin)
        plugin = self._plugins.pop(name, None)
        if plugin is None:
            return None
        for caller in self.hook._hooks.values():
            caller.impls = [i for i in caller.impls if i['fn'].__self__ is not plugin
                            if hasattr(i['fn'], '__self__')]
        return plugin

    def set_blocked(self, name: str) -> None:
        self._blocked.add(name)
        if name in self._plugins:
            self.unregister(name=name)

    def is_blocked(self, name: str) -> bool:
        return name in self._blocked

    def has_plugin(self, name: str) -> bool:
        return name in self._plugins

    def get_plugin(self, name: str):
        return self._plugins.get(name)

    def get_plugins(self) -> list:
        return list(self._plugins.values())

    def list_plugin_distinfo(self) -> list:
        return []

    def list_name_plugin(self) -> list:
        return list(self._plugins.items())

    def add_hookcall_monitoring(self, before, after) -> None:
        pass

    def add_hookimpl_opts(self, opts) -> None:
        pass


def _pluginname(plugin) -> str:
    if hasattr(plugin, '__name__'):
        return plugin.__name__
    return type(plugin).__name__
