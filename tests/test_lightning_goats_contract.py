import json
import os
import uuid

import pytest
import requests
from nostr_sdk import Keys
from pyln.testing.fixtures import *  # noqa: F403
from pyln.testing.utils import wait_for
from util import get_plugin  # noqa: F401


def test_per_address_policy_and_trusted_labels(node_factory, get_plugin):  # noqa: F811
    port = node_factory.get_unused_port()
    url = f"localhost:{port}"
    l1, l2 = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {"log-level": "debug"},
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "clnaddress-listen": url,
                "clnaddress-base-url": f"http://{url}/",
                "clnaddress-min-receivable": 2,
                "clnaddress-max-receivable": 10_000,
            },
        ],
    )
    wait_for(lambda: l2.daemon.is_in_log("Starting lnurlp server."))

    herd = l2.rpc.call(
        "clnaddress-adduser",
        {
            "user": "herd",
            "description": "Feed the Lightning Goats",
            "min_sendable_msat": 1_000,
            "max_sendable_msat": 5_000,
            "comment_allowed": 5,
            "nostr_enabled": False,
        },
    )
    assert herd["user"] == "herd"
    assert herd["min_sendable_msat"] == 1_000
    assert herd["max_sendable_msat"] == 5_000
    assert herd["comment_allowed"] == 5
    assert herd["nostr_enabled"] is False

    l2.rpc.call(
        "clnaddress-adduser",
        {
            "user": "donate",
            "description": "General donations",
            "min_sendable_msat": 2_000,
            "max_sendable_msat": 8_000,
        },
    )

    herd_config = requests.get(f"http://{url}/.well-known/lnurlp/herd").json()
    assert herd_config["minSendable"] == 1_000
    assert herd_config["maxSendable"] == 5_000
    assert herd_config["commentAllowed"] == 5
    assert herd_config["allowsNostr"] is False
    assert "nostrPubkey" not in herd_config

    herd_callback = herd_config["callback"]
    assert requests.get(herd_callback, params={"amount": 999}).status_code == 400
    assert requests.get(herd_callback, params={"amount": 5_001}).status_code == 400
    assert (
        requests.get(
            herd_callback,
            params={"amount": 2_340, "comment": "goats!"},
        ).status_code
        == 400
    )

    response_invoice = requests.get(
        herd_callback,
        params={"amount": 2_340, "comment": "goats"},
    )
    assert response_invoice.status_code == 200
    invstring = response_invoice.json()["pr"]
    l1.rpc.call("xpay", {"invstring": invstring})
    invoice = l2.rpc.call("listinvoices", {"invstring": invstring})["invoices"][0]
    assert invoice["status"] == "paid"
    assert invoice["amount_received_msat"] == 2_340
    assert json.loads(invoice["description"]) == [
        ["text/plain", "Feed the Lightning Goats"],
        ["text/identifier", f"herd@{url}"],
    ]
    label_parts = invoice["label"].split(":")
    assert label_parts[:3] == ["clnaddress", "v1", "herd"]
    assert len(label_parts) == 4
    uuid.UUID(label_parts[3])

    donate_config = requests.get(f"http://{url}/.well-known/lnurlp/donate").json()
    donate_invoice = requests.get(donate_config["callback"], params={"amount": 3_000})
    assert donate_invoice.status_code == 200
    donate_invstring = donate_invoice.json()["pr"]
    l1.rpc.call("xpay", {"invstring": donate_invstring})
    invoice = l2.rpc.call("listinvoices", {"invstring": donate_invstring})["invoices"][0]
    label_parts = invoice["label"].split(":")
    assert label_parts[:3] == ["clnaddress", "v1", "donate"]
    assert len(label_parts) == 4
    uuid.UUID(label_parts[3])

    direct_config = requests.get(f"http://{url}/lnurlp").json()
    direct_invoice = requests.get(direct_config["callback"], params={"amount": 2})
    assert direct_invoice.status_code == 200
    direct_invstring = direct_invoice.json()["pr"]
    invoice = l2.rpc.call("listinvoices", {"invstring": direct_invstring})["invoices"][0]
    assert invoice["label"].startswith("clnaddress:v1:__direct__:")

    with pytest.raises(Exception):
        l2.rpc.call("clnaddress-adduser", {"user": "Herd"})
    with pytest.raises(Exception):
        l2.rpc.call("clnaddress-adduser", {"user": "herd/other"})
    with pytest.raises(Exception):
        l2.rpc.call("clnaddress-adduser", {"user": "another", "comment_allowd": 5})


def test_zap_key_file_and_per_user_nostr_policy(node_factory, get_plugin, tmp_path):  # noqa: F811
    zapper_keys = Keys.generate()
    key_file = tmp_path / "clnaddress-zap.key"
    key_file.write_text(zapper_keys.secret_key().to_hex())
    os.chmod(key_file, 0o600)

    port = node_factory.get_unused_port()
    url = f"localhost:{port}"
    _, node = node_factory.line_graph(
        2,
        wait_for_announce=True,
        opts=[
            {"log-level": "debug"},
            {
                "log-level": "debug",
                "plugin": get_plugin,
                "clnaddress-listen": url,
                "clnaddress-base-url": f"http://{url}/",
                "clnaddress-nostr-privkey-file": str(key_file),
            },
        ],
    )
    wait_for(lambda: node.daemon.is_in_log("Starting lnurlp server."))

    node.rpc.call("clnaddress-adduser", {"user": "herd", "nostr_enabled": True})
    node.rpc.call("clnaddress-adduser", {"user": "quiet", "nostr_enabled": False})

    direct = requests.get(f"http://{url}/lnurlp").json()
    assert direct["allowsNostr"] is True
    assert direct["nostrPubkey"] == zapper_keys.public_key().to_hex()

    herd = requests.get(f"http://{url}/.well-known/lnurlp/herd").json()
    assert herd["allowsNostr"] is True
    assert herd["nostrPubkey"] == zapper_keys.public_key().to_hex()

    quiet = requests.get(f"http://{url}/.well-known/lnurlp/quiet").json()
    assert quiet["allowsNostr"] is False
    assert "nostrPubkey" not in quiet
