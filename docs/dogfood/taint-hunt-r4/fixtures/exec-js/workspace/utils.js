const run = require("./run");

function ping(host) {
  const target = host.trim();
  return run.ping(target);
}

function archive(file) {
  const name = file;
  return run.tar(name);
}

function listDir(dir) {
  return run.ls(dir);
}

function slug(name) {
  const clean = String(name).replace(/[^A-Za-z0-9_-]/g, "");
  return run.echo(clean);
}

function version() {
  return run.version();
}

module.exports = { ping, archive, listDir, slug, version };