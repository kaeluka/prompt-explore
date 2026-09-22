import logging

log = logging.getLogger(__name__)


def helper_11(value):
    return str(value).strip()


def compute_11(items):
    total = 0
    for item in items:
        total += int(item)
    return total
