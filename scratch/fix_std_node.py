import os
import re

def transform_file(path):
    with open(path, 'r') as f:
        content = f.read()

    # Simple replacements
    content = content.replace('use std::sync::Arc;', 'use alloc::sync::Arc;')
    content = content.replace('use std::sync::Mutex;', 'use spin::Mutex;')
    content = content.replace('use std::sync::RwLock;', 'use spin::RwLock;')
    content = content.replace('use std::time::Duration;', 'use core::time::Duration;')
    content = content.replace('use std::collections::VecDeque;', 'use alloc::collections::VecDeque;')
    content = content.replace('use std::collections::HashMap;', 'use hashbrown::HashMap;')
    content = content.replace('use std::collections::HashSet;', 'use hashbrown::HashSet;')
    content = content.replace('use std::collections::BTreeMap;', 'use alloc::collections::BTreeMap;')
    content = content.replace('use std::collections::BTreeSet;', 'use alloc::collections::BTreeSet;')
    content = content.replace('use std::string::String;', 'use alloc::string::String;')
    content = content.replace('use std::vec::Vec;', 'use alloc::vec::Vec;')
    content = content.replace('use std::boxed::Box;', 'use alloc::boxed::Box;')
    content = content.replace('use std::fmt;', 'use core::fmt;')
    content = content.replace('use std::mem;', 'use core::mem;')
    content = content.replace('use std::ptr;', 'use core::ptr;')
    content = content.replace('use std::cmp;', 'use core::cmp;')
    content = content.replace('use std::ops;', 'use core::ops;')
    content = content.replace('use std::iter;', 'use core::iter;')
    content = content.replace('use std::sync::atomic', 'use core::sync::atomic')
    content = content.replace('use std::sync::LazyLock;', 'use once_cell::sync::Lazy as LazyLock;')
    content = content.replace('use std::path::PathBuf;', 'use alloc::string::String as PathBuf;')
    content = content.replace('use std::net::Ipv6Addr;', '// use std::net::Ipv6Addr;')
    
    # Custom replacements for rsnano
    content = content.replace('use bounded_vec_deque::BoundedVecDeque;', 'use crate::utils::BoundedVecDeque;')
    content = content.replace('use rsnano_nullable_lmdb::WriteTransaction;', 'use rsnano_ledger_store::WriteTransaction;') # placeholder
    content = content.replace('use uuid::Uuid;', '// use uuid::Uuid;')
    content = content.replace('use rustc_hash::FxHashMap;', 'use hashbrown::HashMap as FxHashMap;')
    content = content.replace('use rustc_hash::FxHashSet;', 'use hashbrown::HashSet as FxHashSet;')
    content = content.replace('use rsnano_store_lmdb', '// use rsnano_store_lmdb')
    
    # Threading and Panic
    content = content.replace('std::thread::', '// std::thread::')
    content = content.replace('std::panic!', 'panic!')
    content = content.replace('std::process::', '// std::process::')

    # Handle block imports: use std::collections::{...};
    def replace_submod_block(match):
        submod = match.group(1)
        block = match.group(2)
        items = [i.strip() for i in block.split(',')]
        items = [i for i in items if i]
        
        if submod == 'collections':
            hashbrown_items = [i for i in items if i in ['HashMap', 'HashSet']]
            alloc_items = [i for i in items if i in ['VecDeque', 'BTreeMap', 'BTreeSet']]
            res = []
            if hashbrown_items: res.append(f"use hashbrown::{{ {', '.join(hashbrown_items)} }};")
            if alloc_items: res.append(f"use alloc::collections::{{ {', '.join(alloc_items)} }};")
            return '\n'.join(res)
        elif submod == 'sync':
            spin_items = [i for i in items if i in ['Mutex', 'RwLock', 'Once', 'Barrier', 'Condvar']]
            alloc_sync_items = [i for i in items if i == 'Arc']
            core_atomic_items = [i for i in items if i in ['AtomicBool', 'AtomicUsize', 'AtomicU64', 'AtomicI64', 'Ordering']]
            res = []
            if spin_items: res.append(f"use spin::{{ {', '.join(spin_items)} }};")
            if alloc_sync_items: res.append(f"use alloc::sync::{{ {', '.join(alloc_sync_items)} }};")
            if core_atomic_items: res.append(f"use core::sync::atomic::{{ {', '.join(core_atomic_items)} }};")
            return '\n'.join(res)
        elif submod in ['mem', 'cmp', 'ops', 'fmt', 'ptr', 'iter', 'time']:
            return f"use core::{submod}::{{ {', '.join(items)} }};"
        elif submod == 'path':
            return "use alloc::string::String as PathBuf;"
        else:
            return f"// use std::{submod}::{{ {', '.join(items)} }};"

    content = re.sub(r'use std::(\w+)::\{(.*?)\};', replace_submod_block, content, flags=re.DOTALL)

    # Handle generic std block: use std::{...};
    def replace_std_block(match):
        block = match.group(1)
        # Simplified: just comment it out if not handled by above
        return f"// use std::{{ {block} }};"

    content = re.sub(r'use std::\{(.*?)\};', replace_std_block, content, flags=re.DOTALL)

    content = content.replace('std::sync::Arc', 'Arc')
    content = content.replace('std::time::Duration', 'Duration')
    content = content.replace('std::env::var', 'crate::utils::env_var_stub')
    content = content.replace('std::fs::', '// std::fs::')

    with open(path, 'w') as f:
        f.write(content)

def main():
    for root, dirs, files in os.walk('node':
        for file in files:
            if file.endswith('.rs'):
                transform_file(os.path.join(root, file))

if __name__ == '__main__':
    main()
