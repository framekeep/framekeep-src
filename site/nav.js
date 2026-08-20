/* Framekeep — the two courtesies <details> does not give you.
 *
 * The nav menus are <details>/<summary>, so they already open on click and on
 * Enter, and they work on the pages that carry no other script. This file adds
 * only what the element leaves out: Escape closes the open one, clicking
 * anywhere outside closes it, and opening one closes the other.
 *
 * Everything here degrades to "the menus still work, they just stay open until
 * you click the summary again" -- which is why it is a separate file that any
 * page can drop without losing navigation.
 */
(function () {
  'use strict';

  var menus = document.querySelectorAll('.menu');
  if (!menus.length) return;

  function closeAll(except) {
    for (var i = 0; i < menus.length; i++) {
      if (menus[i] !== except) menus[i].removeAttribute('open');
    }
  }

  for (var i = 0; i < menus.length; i++) {
    menus[i].addEventListener('toggle', function () {
      if (this.hasAttribute('open')) closeAll(this);
    });
  }

  document.addEventListener('click', function (e) {
    // `closest` walks up from whatever was clicked; inside a menu it finds the
    // <details>, so a click on a menu item does not close its own menu before
    // the link has been followed.
    if (!e.target.closest || !e.target.closest('.menu')) closeAll(null);
  });

  document.addEventListener('keydown', function (e) {
    if (e.key !== 'Escape') return;
    var open = document.querySelector('.menu[open]');
    if (!open) return;
    open.removeAttribute('open');
    var s = open.querySelector('summary');
    if (s) s.focus();          // put the caret back where the person left it
  });
})();
