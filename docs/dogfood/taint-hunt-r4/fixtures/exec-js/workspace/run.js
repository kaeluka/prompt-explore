const { exec, execSync, execFile } = require("child_process");

const VERSION = "1.4.2";

function ping(target) {
  return exec(`ping -c 1 ${target}`, (err, stdout) => stdout);
}

function tar(name) {
  return execSync("tar -cf /tmp/out.tar " + name);
}

function ls(dir) {
  return execFile("ls", ["-l", dir], (err, stdout) => stdout);
}

function echo(text) {
  return exec(`echo ${text}`);
}

function version() {
  return execSync("node -v").toString();
}

module.exports = { ping, tar, ls, echo, version };