from enum import Flag, auto


class Perm(Flag):
    READ = auto()
    WRITE = auto()
    EXEC = auto()


print(Perm.READ.value, Perm.WRITE.value, Perm.EXEC.value)
rw = Perm.READ | Perm.WRITE
print(Perm.READ in rw)
print(Perm.EXEC in rw)
