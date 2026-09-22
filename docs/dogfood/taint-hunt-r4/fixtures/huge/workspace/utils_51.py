import logging

log = logging.getLogger(__name__)


def helper_51(value):
    return str(value).strip()


def compute_51(items):
    return sum(int(x) for x in items)
