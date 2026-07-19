;;; moosedev.el --- MOOSEDev Knowledge-LSP registrations -*- lexical-binding: t; -*-

;; Copyright (C) 2026 Trivyn
;; SPDX-License-Identifier: Apache-2.0
;; Package-Requires: ((emacs "29.1"))
;; Keywords: tools, languages
;; URL: https://github.com/Trivyn/moosedev

;;; Commentary:

;; MOOSEDev Knowledge-LSP registration for Emacs (spec §5.7: a thin client
;; stanza — no logic of its own). The server is the `moosedev' binary's stdio
;; shim; it relays to the shared daemon and autospawns it on first use.
;;
;; Two entry points are provided; load this file and use whichever LSP
;; client you already run:
;;
;; - eglot (built into Emacs 29+): the zero-install baseline, started
;;   DELIBERATELY via `M-x moosedev-eglot' in a `.moosedev' project. Hover
;;   dossiers, knowledge diagnostics (flymake), and proposal code actions
;;   work; code lenses do not (eglot has no codeLens support — GNU
;;   bug#73452). There is no global `eglot-server-programs' registration:
;;   built-in per-language entries would shadow it anyway, `C-u M-x eglot'
;;   prompts for a raw command (bypassing initialization options), and a
;;   mode-keyed entry would launch MOOSEDev in unrelated projects. NOTE:
;;   eglot runs ONE server per buffer, so `moosedev-eglot' replaces
;;   rust-analyzer/pyright there — an eglot design limit, not MOOSEDev's.
;;
;; - lsp-mode: the full-featured path. Registered with `:add-on? t' so
;;   MOOSEDev runs ALONGSIDE the language server, and code lenses render
;;   (`lsp-lens-enable' is on by default since lsp-mode 9.0). Activation is
;;   gated on the `.moosedev' root marker.
;;
;; No per-language mode list: activation is gated by the `.moosedev' root
;; marker, and the server is silent for files outside its indexing substrate
;; — so new substrate languages need no client edit (the supported set lives
;; in src/code/substrate/lang/).
;;
;; Version floors: pull diagnostics need eglot >= 1.20 (older eglot still
;; works — the daemon pushes for clients that do not advertise pull) or
;; lsp-mode >= 10; UTF-8 positions need eglot >= 1.12 or lsp-mode >= 10.
;; Code-lens/code-action commands (`moosedev.openEntity') are handled
;; server-side: the daemon opens the workbench via window/showDocument, which
;; both clients support.

;;; Code:

;; Both clients are optional; eglot is required at `moosedev-eglot' time and
;; the lsp-mode registration lives inside `with-eval-after-load', so only
;; declare their names for the compiler.
(defvar eglot-server-programs)
(declare-function eglot "eglot")
(declare-function lsp-register-client "lsp-mode")
(declare-function lsp-stdio-connection "lsp-mode")
(declare-function make-lsp-client "lsp-mode")

(defgroup moosedev nil
  "MOOSEDev Knowledge-LSP client."
  :group 'tools
  :prefix "moosedev-")

(defcustom moosedev-lsp-binary "moosedev"
  "The moosedev binary. A bare name resolves from `exec-path'."
  :type 'string)

(defcustom moosedev-lsp-initialization-options
  '(:diagnostics (:constraints t :staleRationale t)
    :codeLens t
    :nudge t)
  "Initialization options passed to the Knowledge-LSP.
Must mirror the server's InitializationOptions shape (src/lsp/mod.rs)."
  :type 'plist)

(defun moosedev--project-root ()
  "The dominating directory carrying `.moosedev', if any."
  (locate-dominating-file default-directory ".moosedev"))

;;; eglot ---------------------------------------------------------------

;;;###autoload
(defun moosedev-eglot ()
  "Start (or reuse) the MOOSEDev Knowledge-LSP for this buffer via eglot.

A deliberate entry point instead of an `eglot-server-programs'
registration: mode-keyed entries are shadowed by the built-in
per-language ones, would launch MOOSEDev in projects without a
`.moosedev' root, and `C-u M-x eglot' cannot select an entry (it
prompts for a raw command, bypassing the initialization options).

eglot runs one server per buffer, so this REPLACES the language
server there; use lsp-mode for the co-resident setup."
  (interactive)
  (require 'eglot)
  (unless (moosedev--project-root)
    (user-error "No `.moosedev' project root above %s" default-directory))
  ;; Shadow the registry with exactly one entry for this buffer's mode, so
  ;; eglot's own guess resolves to MOOSEDev with its initialization options.
  (let ((eglot-server-programs
         `((,major-mode
            . (,moosedev-lsp-binary "lsp"
               :initializationOptions ,moosedev-lsp-initialization-options)))))
    (call-interactively #'eglot)))

;;; lsp-mode ------------------------------------------------------------

(with-eval-after-load 'lsp-mode
  (lsp-register-client
   (make-lsp-client
    :new-connection (lsp-stdio-connection
                     (lambda () (list moosedev-lsp-binary "lsp")))
    :server-id 'moosedev
    ;; The knowledge layer accompanies the language server instead of
    ;; competing with it.
    :add-on? t
    :priority -2
    :activation-fn (lambda (_filename _mode)
                     (and (derived-mode-p 'prog-mode)
                          (moosedev--project-root)))
    :initialization-options (lambda () moosedev-lsp-initialization-options)))
  ;; `moosedev.openEntity' needs no :action-handlers entry: the server lists
  ;; it in executeCommandProvider, so lsp-mode forwards the invocation via
  ;; workspace/executeCommand and the daemon opens the workbench itself.
  )

(provide 'moosedev)

;;; moosedev.el ends here
