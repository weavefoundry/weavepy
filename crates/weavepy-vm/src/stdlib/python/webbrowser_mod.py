"""``webbrowser`` — launch the default browser to view a URL.

We ship the small surface real users actually call:

    webbrowser.open(url)
    webbrowser.open_new(url)
    webbrowser.open_new_tab(url)
    webbrowser.get(using=None)
    webbrowser.register(name, klass, instance, preferred=False)

The deep per-browser ``Mozilla``/``Galeon``/``Konqueror`` classes
CPython ships are intentionally absent; ``webbrowser.get("firefox")``
falls back to invoking ``firefox`` via the PATH.
"""

import os
import subprocess
import sys


__all__ = ['open', 'open_new', 'open_new_tab', 'get', 'register',
            'Error', 'BackgroundBrowser', 'GenericBrowser']


class Error(Exception):
    pass


_browsers = {}
_tryorder = []


class BaseBrowser:
    def __init__(self, name=''):
        self.name = name
        self.basename = name

    def open(self, url, new=0, autoraise=True):
        raise NotImplementedError

    def open_new(self, url):
        return self.open(url, 1)

    def open_new_tab(self, url):
        return self.open(url, 2)


class GenericBrowser(BaseBrowser):
    """Runs an external program, passing the URL as an argument."""

    def __init__(self, name):
        if isinstance(name, str):
            self.name = name
            self.args = [name, '%s']
        else:
            self.name = name[0]
            self.args = list(name)
        self.basename = os.path.basename(self.name)

    def open(self, url, new=0, autoraise=True):
        cmdline = [arg.replace('%s', url) for arg in self.args]
        try:
            if sys.platform[:3] == 'win':
                subprocess.Popen(cmdline)
            else:
                subprocess.Popen(cmdline, close_fds=True)
            return True
        except OSError:
            return False


class BackgroundBrowser(GenericBrowser):
    def open(self, url, new=0, autoraise=True):
        cmdline = [arg.replace('%s', url) for arg in self.args]
        try:
            subprocess.Popen(cmdline, close_fds=True)
            return True
        except OSError:
            return False


def register(name, klass, instance=None, *, preferred=False):
    _browsers[name.lower()] = [klass, instance]
    if preferred:
        _tryorder.insert(0, name)
    else:
        _tryorder.append(name)


def get(using=None):
    if using is None:
        for name in _tryorder:
            browser = _browsers.get(name.lower())
            if browser is None:
                continue
            klass, instance = browser
            if instance is not None:
                return instance
            return klass()
        return _default_browser()
    name = using.lower()
    browser = _browsers.get(name)
    if browser is not None:
        klass, instance = browser
        return instance if instance is not None else klass()
    # Fall back to searching PATH.
    return GenericBrowser([using])


def _default_browser():
    if sys.platform == 'darwin':
        return GenericBrowser(['open', '%s'])
    if sys.platform[:3] == 'win':
        return GenericBrowser(['cmd', '/c', 'start', '', '%s'])
    return GenericBrowser(['xdg-open', '%s'])


def open(url, new=0, autoraise=True):
    return get().open(url, new, autoraise)


def open_new(url):
    return get().open_new(url)


def open_new_tab(url):
    return get().open_new_tab(url)


# Pre-register a handful of common names.
if sys.platform == 'darwin':
    register('macosx', None, GenericBrowser(['open', '%s']), preferred=True)
elif sys.platform[:3] == 'win':
    register('windows-default', None,
                GenericBrowser(['cmd', '/c', 'start', '', '%s']),
                preferred=True)
else:
    register('xdg-open', None, GenericBrowser(['xdg-open', '%s']),
                preferred=True)
register('firefox', GenericBrowser, GenericBrowser(['firefox', '%s']))
register('chrome', GenericBrowser, GenericBrowser(['google-chrome', '%s']))
register('safari', GenericBrowser, GenericBrowser(['safari', '%s']))
