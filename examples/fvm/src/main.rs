use std::mem;

use fvm_shared::blockstore::{Blockstore, MemoryBlockstore};
use cid::Cid;
use fvm;
// TODO: the compiler doesn't bring inherited traits in scope automatically when
//  importing a supertrait :-/
use fvm::kernel::MessageOps;
use fvm::syscalls::bind_syscalls;
use fvm::{DefaultKernel, Kernel};
use multihash::{Code as MhCode, MultihashDigest};
use std::convert::TryInto;
use wasmtime::{
    Config, Engine, Global, GlobalType, Linker, Module, Mutability, Store, Val, ValType,
};

mod metadata;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let module_wasm = include_bytes!("../fvm_example_actor.wasm");
    let config = fvm::Config {
        initial_pages: 0,
        max_pages: 10,
        engine: Default::default(),
    };
    let mut engine = Engine::new(&config.engine)?;
    let module = Module::new(&engine, module_wasm)?;

    let bs = MemoryBlockstore::default();
    let root_block = b"test root block";
    let root_cid = Cid::new_v1(0x55, MhCode::Sha2_256.digest(root_block));
    bs.put(&root_cid, root_block)?;

    // Create a new linker and bind the syscalls to it.
    let mut linker = Linker::new(&engine);
    bind_syscalls(&mut linker)?;

    // Instantiate a new kernel for this invocation.
    let kernel = DefaultKernel::new(bs, root_cid);

    // Place the above kernel inside a wasmtime Store. Instantiate the module
    // with this store.
    let mut store = Store::new(&engine, kernel);
    let instance = linker.instantiate(&mut store, &module)?;

    if let Some(meta_global) = instance.get_export(&mut store, "meta1") {
        // TODO: consider a better versioning system? This one means we either need to iterate
        // through all exports, or check each metaN.
        let meta_addr = meta_global
            .into_global()
            .ok_or("meta1 should have been a global")?
            .get(&mut store)
            .i32()
            .ok_or("meta1 should have been an address")?;

        let memory = instance
            .get_export(&mut store, "memory")
            .and_then(|m| m.into_memory())
            .ok_or("expected memory export")?;

        let metadata = {
            let k = store.data();
            metadata::Metadata1 {
                value_received: k.value_received().into(),
                method: k.method_number(),
                caller: k.caller(),
                receiver: k.receiver(),
                epoch: 0,           // TODO
                network_version: 0, // TODO
            }
        };

        let mem_size: i32 = memory
            .data_size(&store)
            .try_into()
            .map_err(|_| "memory too large")?;
        if mem_size < meta_addr || (mem_size - meta_addr) < mem::size_of_val(&metadata) as i32 {
            return Err("invalid metadata offsets".into());
        }

        unsafe { *(memory.data_ptr(&store) as *mut metadata::Metadata1) = metadata }
    }

    let method_params = store.data().method_params();
    let invoke = instance.get_typed_func(&mut store, "invoke")?;
    let (result,): (u32,) = invoke.call(&mut store, (method_params,))?;
    println!("{:?}", result);
    Ok(())
}
