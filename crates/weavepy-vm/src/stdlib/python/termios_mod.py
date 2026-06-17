"""Minimal `termios` shim.

WeavePy does not (yet) implement real POSIX terminal control. CPython's
`termios` is a C extension; the only reason it shows up in our test runs is
that pure-Python modules like `tty` (and, transitively, `test_asyncio`'s
`test_events`) do ``from termios import *`` at import time and reference the
flag constants. The actual tty-manipulating tests are guarded by
``hasattr(os, 'openpty')`` (which WeavePy doesn't provide) and skip, so we
only need the names to exist and the syscalls to fail cleanly on a
non-terminal fd.

Constant values are the darwin/BSD <termios.h> values; they are not used for
real ioctls here, only for the bit-twiddling in `tty.cfmakeraw`/`cfmakecbreak`.
"""


class error(Exception):
    pass


# c_iflag bits
IGNBRK = 0x00000001
BRKINT = 0x00000002
IGNPAR = 0x00000004
PARMRK = 0x00000008
INPCK = 0x00000010
ISTRIP = 0x00000020
INLCR = 0x00000040
IGNCR = 0x00000080
ICRNL = 0x00000100
IXON = 0x00000200
IXOFF = 0x00000400
IXANY = 0x00000800
IMAXBEL = 0x00002000

# c_oflag bits
OPOST = 0x00000001
ONLCR = 0x00000002
OXTABS = 0x00000004
ONOEOT = 0x00000008

# c_cflag bits
CSIZE = 0x00000300
CS5 = 0x00000000
CS6 = 0x00000100
CS7 = 0x00000200
CS8 = 0x00000300
CSTOPB = 0x00000400
CREAD = 0x00000800
PARENB = 0x00001000
PARODD = 0x00002000
HUPCL = 0x00004000
CLOCAL = 0x00008000

# c_lflag bits
ECHOKE = 0x00000001
ECHOE = 0x00000002
ECHOK = 0x00000004
ECHO = 0x00000008
ECHONL = 0x00000010
ECHOPRT = 0x00000020
ECHOCTL = 0x00000040
ISIG = 0x00000080
ICANON = 0x00000100
IEXTEN = 0x00000400
EXTPROC = 0x00000800
TOSTOP = 0x00400000
FLUSHO = 0x00800000
NOKERNINFO = 0x02000000
PENDIN = 0x20000000
NOFLSH = 0x80000000

# c_cc array indices (darwin layout)
VEOF = 0
VEOL = 1
VEOL2 = 2
VERASE = 3
VWERASE = 4
VKILL = 5
VREPRINT = 6
VINTR = 8
VQUIT = 9
VSUSP = 10
VDSUSP = 11
VSTART = 12
VSTOP = 13
VLNEXT = 14
VDISCARD = 15
VMIN = 16
VTIME = 17
NCCS = 20

# tcsetattr() `when` values
TCSANOW = 0
TCSADRAIN = 1
TCSAFLUSH = 2
TCSASOFT = 0x10

# tcflush() queue selectors
TCIFLUSH = 1
TCOFLUSH = 2
TCIOFLUSH = 3

# tcflow() actions
TCOOFF = 1
TCOON = 2
TCIOFF = 3
TCION = 4

B0 = 0
B9600 = 9600
B38400 = 38400
B115200 = 115200


def tcgetattr(fd):
    raise error(25, "Inappropriate ioctl for device")


def tcsetattr(fd, when, attributes):
    raise error(25, "Inappropriate ioctl for device")


def tcsendbreak(fd, duration):
    raise error(25, "Inappropriate ioctl for device")


def tcdrain(fd):
    raise error(25, "Inappropriate ioctl for device")


def tcflush(fd, queue):
    raise error(25, "Inappropriate ioctl for device")


def tcflow(fd, action):
    raise error(25, "Inappropriate ioctl for device")


def tcgetwinsize(fd):
    raise error(25, "Inappropriate ioctl for device")


def tcsetwinsize(fd, winsize):
    raise error(25, "Inappropriate ioctl for device")
