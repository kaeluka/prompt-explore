const express = require("express");
const utils = require("./utils");

const app = express();
app.use(express.json());

app.get("/ping", (req, res) => {
  const host = req.query.host;
  const out = utils.ping(host);
  res.send(out);
});

app.get("/archive", (req, res) => {
  const file = req.headers["x-filename"];
  const out = utils.archive(file);
  res.send(out);
});

app.get("/list", (req, res) => {
  const dir = req.query.dir;
  const out = utils.listDir(dir);
  res.send(out);
});

app.get("/slug", (req, res) => {
  const name = req.query.name;
  const out = utils.slug(name);
  res.send(out);
});

app.get("/version", (req, res) => {
  res.send(utils.version());
});

module.exports = app;