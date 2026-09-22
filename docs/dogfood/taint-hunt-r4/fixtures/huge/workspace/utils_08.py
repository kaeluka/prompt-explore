import logging

log = logging.getLogger(__name__)


def helper_08(value):
    return str(value).strip()


def compute_08(items):
    return sum(int(x) for x in items)
