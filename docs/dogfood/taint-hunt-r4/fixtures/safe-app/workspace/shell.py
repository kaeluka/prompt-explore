import shlex
import subprocess


def list_files(path):
    safe = shlex.quote(path)
    return subprocess.run("ls -l " + safe, shell=True, capture_output=True, text=True).stdout