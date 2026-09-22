from flask import Flask, request
import plugins

app = Flask(__name__)


@app.route("/search")
def search():
    q = request.args.get("q")
    return str(plugins.handle(q))


@app.route("/exact")
def exact():
    term = request.args.get("term")
    return str(plugins.handle_exact(term))