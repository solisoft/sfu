// Copy buttons on every code block. The pre is an overflow container, so the
// button lives on a position:relative wrapper instead - it must not scroll
// away with wide code.
(function () {
  document.querySelectorAll('.doc pre').forEach(function (pre) {
    var wrap = document.createElement('div');
    wrap.className = 'codewrap';
    // the language tab renders on the wrapper so it can't be clipped or
    // scrolled away by the pre's own overflow
    if (pre.dataset.lang) {
      wrap.dataset.lang = pre.dataset.lang;
      pre.removeAttribute('data-lang');
    }
    pre.parentNode.insertBefore(wrap, pre);
    wrap.appendChild(pre);

    var btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'copybtn';
    btn.textContent = 'copy';
    btn.title = 'Copy to clipboard';
    btn.addEventListener('click', function () {
      var code = pre.querySelector('code');
      var text = (code ? code.innerText : pre.innerText).replace(/\n+$/, '');
      navigator.clipboard.writeText(text).then(function () {
        btn.textContent = 'copied';
        btn.classList.add('ok');
        setTimeout(function () { btn.textContent = 'copy'; btn.classList.remove('ok'); }, 1200);
      }).catch(function () {
        btn.textContent = 'failed';
        setTimeout(function () { btn.textContent = 'copy'; }, 1200);
      });
    });
    wrap.appendChild(btn);
  });
})();
