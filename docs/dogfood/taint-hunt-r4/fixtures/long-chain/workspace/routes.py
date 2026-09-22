from flask import Flask, request
import service

app = Flask(__name__)


@app.route("/lookup")
def lookup():
    user_in = request.args.get("user")
    return str(service.handle(user_in))


@app.route("/audit")
def audit():
    key = request.headers.get("X-Key")
    service.record(key)
    return "ok"


@app.route("/echo")
def echo():
    text = request.args.get("text")
    return str(service.show(text))