import json
import threading
import time

import severin

app = severin.App(width=900, height=650)

def handle_request(receipt, json_text):
    message = json.loads(json_text)
    if message.get("kind") == "deferred":
        def later():
            time.sleep(message.get("delayMs", 2500) / 1000)
            app.write(receipt, json.dumps({"kind": "deferred-reply", "receipt": receipt}))
        threading.Thread(target=later, daemon=True).start()
        return None
    return json.dumps({"kind": "immediate-reply", "receipt": receipt, "echo": message})

app.bridge = handle_request
app.load_path(__file__.replace("run_demo.py", "index.html"))
app.run()
