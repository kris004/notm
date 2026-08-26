#!/usr/bin/env python3
"""Parse one captured RFC 5322 message with Python's independent MIME stack."""

import hashlib
import json
import sys
from email import policy
from email.parser import BytesParser
from email.utils import getaddresses
from pathlib import Path


def addresses(message, name):
    values = [str(value) for value in message.get_all(name, [])]
    return [{"name": display, "address": address} for display, address in getaddresses(values)]


def part_summary(part):
    summary = {
        "content_type": part.get_content_type(),
        "content_transfer_encoding": str(part.get("Content-Transfer-Encoding", "")),
        "disposition": part.get_content_disposition(),
        "filename": part.get_filename(),
        "defects": [str(defect) for defect in part.defects],
    }
    if part.get_content_type() == "message/rfc822":
        nested = part.get_payload()
        nested_message = nested[0] if isinstance(nested, list) and nested else None
        if nested_message is not None:
            nested_bytes = nested_message.as_bytes(policy=policy.SMTP)
            summary.update(
                {
                    "size": len(nested_bytes),
                    "sha256": hashlib.sha256(nested_bytes).hexdigest(),
                    "nested_subject": str(nested_message.get("Subject", "")),
                    "nested_message_id": str(nested_message.get("Message-ID", "")),
                }
            )
        return summary
    if part.get_content_maintype() == "multipart":
        return summary

    payload = part.get_payload(decode=True) or b""
    summary.update(
        {
            "size": len(payload),
            "sha256": hashlib.sha256(payload).hexdigest(),
        }
    )
    if part.get_content_maintype() == "text":
        try:
            summary["text"] = part.get_content()
        except (LookupError, UnicodeError) as error:
            summary["text_error"] = str(error)
    return summary


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: parse_wire.py MESSAGE.eml")
    raw = Path(sys.argv[1]).read_bytes()
    message = BytesParser(policy=policy.default).parsebytes(raw)
    result = {
        "subject": str(message.get("Subject", "")),
        "from": addresses(message, "From"),
        "to": addresses(message, "To"),
        "cc": addresses(message, "Cc"),
        "bcc": addresses(message, "Bcc"),
        "message_id": str(message.get("Message-ID", "")),
        "date": str(message.get("Date", "")),
        "in_reply_to": str(message.get("In-Reply-To", "")),
        "references": str(message.get("References", "")),
        "defects": [str(defect) for defect in message.defects],
        "parts": [part_summary(part) for part in message.walk()],
    }
    json.dump(result, sys.stdout, ensure_ascii=False, sort_keys=True)


if __name__ == "__main__":
    main()
