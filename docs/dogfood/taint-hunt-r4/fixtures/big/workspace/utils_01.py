import logging

log = logging.getLogger(__name__)


def helper_01(value):
    return str(value).strip()


def compute_01(items):
    total = 0
    for item in items:
        total += int(item)
    return total
