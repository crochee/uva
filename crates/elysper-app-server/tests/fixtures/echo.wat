;; A minimal WebAssembly component exporting the tool contract:
;;
;;   describe: func() -> string
;;   execute: func(input: string) -> string
;;
;; `describe` returns one fixed JSON schema; `execute` echoes valid
;; JSON. The fixture is written to a tempdir by the `wasm` profile-row
;; test, which mounts it through `WasmToolPlugin`.
(component
  (core module $lib
    (memory (export "memory") 1)
    (global $next (mut i32) (i32.const 8192))
    (data (i32.const 16) "{\"name\":\"echo\",\"description\":\"Echo the JSON input\",\"parameters\":{\"type\":\"object\",\"properties\":{\"value\":{\"type\":\"string\"}}}}")
    (func (export "cabi_realloc")
      (param $old_ptr i32) (param $old_size i32)
      (param $align i32) (param $new_size i32)
      (result i32)
      (local $ptr i32)
      (local $mask i32)
      (local.set $mask (i32.sub (local.get $align) (i32.const 1)))
      (local.set $ptr
        (i32.and
          (i32.add (global.get $next) (local.get $mask))
          (i32.xor (local.get $mask) (i32.const -1))))
      (global.set $next (i32.add (local.get $ptr) (local.get $new_size)))
      (if (i32.ne (local.get $old_size) (i32.const 0))
        (then
          (memory.copy (local.get $ptr) (local.get $old_ptr)
            (if (result i32)
              (i32.lt_u (local.get $old_size) (local.get $new_size))
              (then (local.get $old_size))
              (else (local.get $new_size))))))
      (local.get $ptr))
    (func (export "describe") (result i32)
      (i32.store (i32.const 4096) (i32.const 16))
      (i32.store offset=4 (i32.const 4096) (i32.const 123))
      (i32.const 4096))
    (func (export "execute")
      (param $ptr i32) (param $len i32) (result i32)
      (i32.store (i32.const 4096) (local.get $ptr))
      (i32.store offset=4 (i32.const 4096) (local.get $len))
      (i32.const 4096))
  )
  (core instance $lib_inst (instantiate $lib))
  (func (export "describe") (result string)
    (canon lift (core func $lib_inst "describe")
      (memory (core memory $lib_inst "memory"))
      (realloc (core func $lib_inst "cabi_realloc"))))
  (func (export "execute") (param "input" string) (result string)
    (canon lift (core func $lib_inst "execute")
      (memory (core memory $lib_inst "memory"))
      (realloc (core func $lib_inst "cabi_realloc"))))
)
