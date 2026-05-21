import subprocess

print("--- run / check_output ---")
r = subprocess.run(["echo", "hello", "world"], capture_output=True, text=True)
print("returncode:", r.returncode)
print("stdout:", repr(r.stdout))
print("stderr:", repr(r.stderr))

print("--- check_output ---")
out = subprocess.check_output(["echo", "weave"], text=True)
print("got:", repr(out))

print("--- stdin pipe ---")
r = subprocess.run(["tr", "a-z", "A-Z"], input=b"hello world", capture_output=True)
print("returncode:", r.returncode)
print("stdout:", r.stdout)

print("--- failing command ---")
r = subprocess.run(["false"])
print("returncode:", r.returncode)

print("--- check_call raises ---")
try:
    subprocess.check_call(["false"])
    print("no error")
except subprocess.CalledProcessError as e:
    print("CalledProcessError:", e.returncode)

print("--- env ---")
r = subprocess.run(["sh", "-c", "echo $WEAVE_TEST"],
                   capture_output=True, text=True, env={"WEAVE_TEST": "hi"})
print("got:", repr(r.stdout))
