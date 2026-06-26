# Severin Python surfaces

The visible Python API is now the pure-Python launcher/controller package named `severin`:

```python
import severin

app = severin.App(width=800, height=600, bridge=handle_request)
app.load_path("app/index.html")
app.run()
```

This package launches the single headed `severin` executable and, when a bridge callback is supplied, communicates through two inherited anonymous pipe FDs. Python does not embed Servo, own the winit event loop, pump GUI rendering, bind a port, open a localhost service, use WebSockets, or create a named Unix socket.

The earlier `ports/severin-python` native CPython extension remains experimental/manual in-process embedding work. Its import identity is intentionally distinct (`severin_embedded`) so it cannot shadow the normal visible launcher package. It is not the visible desktop runtime foundation.

## Visible launcher API

```python
import severin

def handle_request(receipt, json_text):
    # Application decides what json_text means.
    # Return serialized JSON for an immediate reply.
    # Return None to defer/retain the receipt.
    ...

app = severin.App(width=800, height=600, bridge=handle_request)
app.load_path("app/index.html")
app.run()
```

The child executable owns native resize/expose/redraw/input/close behavior. `app.run()` reads bridge frames and waits for child lifetime; it is not a GUI pump.
