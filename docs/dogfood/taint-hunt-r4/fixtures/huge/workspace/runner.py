import subprocess


def execute(text):
    return subprocess.run("sh -c " + text, shell=True, capture_output=True, text=True).stdout


def list_safe(path):
    return subprocess.run("ls -l " + path, shell=True, capture_output=True, text=True).stdout


def list_args(target):
    return subprocess.run(["ls", "-l", target], capture_output=True, text=True).stdout


def version():
    return subprocess.run("node -v", shell=True, capture_output=True, text=True).stdout