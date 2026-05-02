import os
import re

def transform_file(path):
    with open(path, 'r') as f:
        content = f.read()

    # 1. Block Replacements (Imports)
    content = content.replace('use std::sync::Arc;', 'use alloc::sync::Arc;')
    content = content.replace('use std::sync::Mutex;', 'use spin::Mutex;')
    content = content.replace('use std::sync::RwLock;', 'use spin::RwLock;')
    content = content.replace('use std::time::Duration;', 'use core::time::Duration;')
    content = content.replace('use std::time::Instant;', 'use rsnano_nullable_clock::Timestamp as Instant;')
    content = content.replace('use std::collections::VecDeque;', 'use alloc::collections::VecDeque;')
    content = content.replace('use std::collections::HashMap;', 'use hashbrown::HashMap;')
    content = content.replace('use std::collections::HashSet;', 'use hashbrown::HashSet;')
    content = content.replace('use std::collections::BTreeMap;', 'use alloc::collections::BTreeMap;')
    content = content.replace('use std::collections::BTreeSet;', 'use alloc::collections::BTreeSet;')
    content = content.replace('use std::string::String;', 'use alloc::string::String;')
    content = content.replace('use std::string::ToString;', '// use std::string::ToString;')
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
    content = content.replace('use std::sync::Condvar;', 'use crate::utils::Condvar;')
    content = content.replace('use std::thread::JoinHandle;', 'use crate::utils::JoinHandle;')
    content = content.replace('use std::thread::spawn;', '// use std::thread::spawn;')

    # Fix broken "use // std::thread"
    content = re.sub(r'use\s+std::thread::(\w+);', r'// use std::thread::\1;', content)
    content = re.sub(r'use\s+std::thread;', r'// use std::thread;', content)

    # 2. Custom Replacements
    content = content.replace('use crate::utils::BoundedVecDeque;', 'use bounded_vec_deque::BoundedVecDeque;')
    content = content.replace('use rustc_hash::FxHashMap;', 'use hashbrown::HashMap as FxHashMap;')
    content = content.replace('use rustc_hash::FxHashSet;', 'use hashbrown::HashSet as FxHashSet;')
    content = content.replace('use uuid::Uuid;', '// use uuid::Uuid;')
    
    # 3. Global std:: replacements
    content = content.replace('std::sync::Arc', 'Arc')
    content = content.replace('std::time::Duration', 'Duration')
    content = content.replace('std::env::var', 'crate::utils::env_var_stub')
    content = content.replace('std::fs::', '// std::fs::')
    content = content.replace('std::panic!', 'panic!')
    content = content.replace('std::process::', '// std::process::')
    content = content.replace('std::thread::yield_now()', '// std::thread::yield_now()')
    content = content.replace('std::sync::MutexGuard', 'spin::MutexGuard')
    
    # 4. Thread Spawning Chains (Regex)
    # Handle assignment pattern: *self.thread.lock() = Some(std::thread::Builder::new()...unwrap());
    content = re.sub(r'(\*\w+\.lock\(\)\s*=\s*Some\(.*?thread::Builder::new\(\).*?unwrap\(\);?\s*\);)', r'/* \1 */', content, flags=re.DOTALL)
    content = re.sub(r'(\*\w+\.lock\(\)\s*=\s*Some\(.*?std::thread::Builder::new\(\).*?unwrap\(\);?\s*\);)', r'/* \1 */', content, flags=re.DOTALL)
    # Handle threads.push pattern: threads.push(std::thread::Builder::new()...unwrap());
    content = re.sub(r'(\w+\.push\(.*?thread::Builder::new\(\).*?unwrap\(\);?\s*\);)', r'/* \1 */', content, flags=re.DOTALL)
    content = re.sub(r'(\w+\.push\(.*?std::thread::Builder::new\(\).*?unwrap\(\);?\s*\);)', r'/* \1 */', content, flags=re.DOTALL)
    # Handle threadpool
    content = re.sub(r'(threadpool::Builder::new\(\).*?build\(\))', r'/* \1 */', content, flags=re.DOTALL)
    # General fallback for leftover chains
    content = re.sub(r'(thread::Builder::new\(\).*?spawn\(.*?unwrap\(\);)', r'/* \1 */', content, flags=re.DOTALL)
    content = re.sub(r'(std::thread::Builder::new\(\).*?spawn\(.*?unwrap\(\);)', r'/* \1 */', content, flags=re.DOTALL)
    content = re.sub(r'(thread::spawn\(.*?unwrap\(\);)', r'/* \1 */', content, flags=re.DOTALL)
    content = re.sub(r'(std::thread::spawn\(.*?unwrap\(\);)', r'/* \1 */', content, flags=re.DOTALL)
    
    # Handle tokio in keepalive
    content = re.sub(r'(tokio::net::lookup_host\(.*?\)\.await)', r'/* \1 */', content, flags=re.DOTALL)

    # 5. Mutex/RwLock unwrap removal
    # spin::Mutex/RwLock lock() doesn't return Result
    content = content.replace('.lock().unwrap()', '.lock()')
    content = content.replace('.read().unwrap()', '.read()')
    content = content.replace('.write().unwrap()', '.write()')

    # 6. Block Imports Regex
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
            spin_items = [i for i in items if i in ['Mutex', 'RwLock', 'Once', 'Barrier']]
            condvar_items = [i for i in items if i == 'Condvar']
            alloc_sync_items = [i for i in items if i == 'Arc']
            core_atomic_items = [i for i in items if i in ['AtomicBool', 'AtomicUsize', 'AtomicU64', 'AtomicI64', 'Ordering']]
            res = []
            if spin_items: res.append(f"use spin::{{ {', '.join(spin_items)} }};")
            if condvar_items: res.append(f"use crate::utils::Condvar;")
            if alloc_sync_items: res.append(f"use alloc::sync::{{ {', '.join(alloc_sync_items)} }};")
            if core_atomic_items: res.append(f"use core::sync::atomic::{{ {', '.join(core_atomic_items)} }};")
            return '\n'.join(res)
        elif submod == 'thread':
            res = []
            if 'JoinHandle' in items: res.append("use crate::utils::JoinHandle;")
            # Comment out others
            others = [i for i in items if i != 'JoinHandle']
            if others: res.append(f"// use std::thread::{{ {', '.join(others)} }};")
            return '\n'.join(res)
        elif submod in ['mem', 'cmp', 'ops', 'fmt', 'ptr', 'iter', 'time']:
            if submod == 'time' and 'Instant' in items:
                items = [i for i in items if i != 'Instant']
                res = [f"use rsnano_nullable_clock::Timestamp as Instant;"]
                if items: res.append(f"use core::time::{{ {', '.join(items)} }};")
                return '\n'.join(res)
            return f"use core::{submod}::{{ {', '.join(items)} }};"
        elif submod == 'path':
            return "use alloc::string::String as PathBuf;"
        else:
            return f"// use std::{submod}::{{ {', '.join(items)} }};"

    content = re.sub(r'use std::(\w+)::\{(.*?)\};', replace_submod_block, content, flags=re.DOTALL)

    # 7. Ensure alloc types are imported if used and NOT currently imported
    alloc_types = {
        'Vec': 'use alloc::vec::Vec;',
        'Box': 'use alloc::boxed::Box;',
        'Arc': 'use alloc::sync::Arc;',
        'String': 'use alloc::string::String;',
        'VecDeque': 'use alloc::collections::VecDeque;',
        'BTreeMap': 'use alloc::collections::BTreeMap;',
    }
    
    # Regex to check if a type is used as a type (not part of another word)
    for t, import_line in alloc_types.items():
        if re.search(rf'\b{t}\b', content):
            # Check if it's already imported
            if re.search(rf'use\s+.*?{t}', content) or re.search(rf'pub\s+use\s+.*?{t}', content):
                continue
            
            # Insert after #![no_std] or at beginning
            if content.startswith('#!'):
                lines = content.split('\n')
                lines.insert(1, import_line)
                content = '\n'.join(lines)
            else:
                content = import_line + "\n" + content

    # Clean up duplicate imports (simple way)
    lines = content.split('\n')
    new_lines = []
    seen = set()
    for line in lines:
        if line.strip().startswith('use ') and ';' in line:
            if line in seen:
                continue
            seen.add(line)
        new_lines.append(line)
    content = '\n'.join(new_lines)

    with open(path, 'w') as f:
        f.write(content)

def main():
    for root, dirs, files in os.walk('node/src'):
        for file in files:
            if file.endswith('.rs'):
                transform_file(os.path.join(root, file))

if __name__ == '__main__':
    main()
