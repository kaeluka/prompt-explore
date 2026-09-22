import logging

log = logging.getLogger(__name__)


def helper_15(value):
    return str(value).strip()


def compute_15(items):
    total = 0
    for item in items:
        total += int(item)
    return total
