// Themed hover tooltip for the fleet-ribbon cells, replacing the native
// `title=` (unthemed, ~1s delay). A single <body> node plus document
// delegation survives the table's 5s swap and the cell's scaleY hover; names
// go in via textContent so a customer monitor name can't inject markup.
(function () {
    "use strict";

    var SEL = ".dashboard-ribbon__seg[data-tip-time]";
    var tip;

    function el(cls, text) {
        var node = document.createElement("div");
        node.className = cls;
        if (text != null) node.textContent = text;
        return node;
    }

    function ensureTip() {
        if (!tip) {
            tip = document.createElement("div");
            tip.className = "sm-ribbon-tip";
            tip.setAttribute("role", "presentation");
            tip.hidden = true;
            document.body.appendChild(tip);
        }
        return tip;
    }

    function build(seg) {
        var t = ensureTip();
        t.textContent = "";

        var head = el("sm-ribbon-tip__head");
        head.appendChild(el("sm-ribbon-tip__time", seg.getAttribute("data-tip-time") || ""));
        head.appendChild(el("sm-ribbon-tip__stat", seg.getAttribute("data-tip-stat") || ""));
        t.appendChild(head);

        var count = parseInt(seg.getAttribute("data-tip-count") || "0", 10);
        if (count > 0) {
            t.appendChild(el(
                "sm-ribbon-tip__count",
                count + (count === 1 ? " monitor down" : " monitors down")
            ));

            var names = seg.querySelectorAll(".ribbon-seg-names > i");
            if (names.length) {
                var list = document.createElement("ul");
                list.className = "sm-ribbon-tip__list";
                for (var i = 0; i < names.length; i++) {
                    var li = document.createElement("li");
                    li.className = "sm-ribbon-tip__name";
                    li.textContent = names[i].textContent;
                    li.title = names[i].textContent;
                    list.appendChild(li);
                }
                t.appendChild(list);
            }
            if (count > names.length) {
                t.appendChild(el("sm-ribbon-tip__more", "+" + (count - names.length) + " more"));
            }
        }
        return t;
    }

    function show(seg) {
        var t = build(seg);
        if (window.smPositionFloating) {
            window.smPositionFloating(seg, t, { gap: 6 });
        } else {
            t.hidden = false;
        }
    }

    function hide() {
        if (tip) tip.hidden = true;
    }

    function segOf(e) {
        return e.target && e.target.closest ? e.target.closest(SEL) : null;
    }

    document.addEventListener("pointerover", function (e) {
        var seg = segOf(e);
        if (seg) show(seg);
    });
    document.addEventListener("pointerout", function (e) {
        if (segOf(e)) hide();
    });
    // Keyboard parity for the focusable drill cells.
    document.addEventListener("focusin", function (e) {
        var seg = segOf(e);
        if (seg) show(seg);
    });
    document.addEventListener("focusout", hide);
    // The 5s swap replaces the cell node mid-hover; re-anchor to whatever cell
    // is under the pointer now (or hide if none) so the tip doesn't blink out.
    if (document.body) {
        document.body.addEventListener("htmx:afterSettle", function () {
            var hovered = document.querySelector(SEL + ":hover");
            if (hovered) show(hovered);
            else hide();
        });
    }
})();
