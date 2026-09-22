from flask import Flask, request
import db
import shell

app = Flask(__name__)


@app.route("/user")
def user():
    uid = request.args.get("uid")
    return str(db.get_user(uid))


@app.route("/search")
def search():
    name = request.args.get("name")
    return str(db.find_by_name(name))


@app.route("/sorted")
def sorted_users():
    col = request.args.get("col")
    return str(db.sorted(col))


@app.route("/files")
def files():
    path = request.args.get("path")
    return shell.list_files(path)