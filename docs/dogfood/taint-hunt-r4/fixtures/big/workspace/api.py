from flask import Flask, request
import middleware
import format_utils
import storage
import runner

app = Flask(__name__)


@app.route("/search")
def search():
    q = request.args.get("q")
    return str(middleware.dispatch(q))


@app.route("/run")
def run():
    cmd = request.args.get("cmd")
    return format_utils.wrap(cmd)


@app.route("/user")
def user():
    uid = request.args.get("uid")
    return str(storage.by_id(uid))


@app.route("/page")
def page():
    slug = request.args.get("slug")
    return str(storage.by_slug(slug))


@app.route("/sort")
def sort():
    col = request.args.get("col")
    return str(storage.sorted(col))


@app.route("/files")
def files():
    path = request.args.get("path")
    return format_utils.quote_and_list(path)


@app.route("/listargs")
def listargs():
    target = request.args.get("target")
    return runner.list_args(target)