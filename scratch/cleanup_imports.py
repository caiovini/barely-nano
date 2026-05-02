import os
import re

def cleanup_duplicates(path):
    with open(path, 'r') as f:
        content = f.read()
    
    lines = content.split('\n')
    new_lines = []
    seen_types = set()
    
    # Track imports of types like Arc, Vec, Box, etc.
    # Pattern: use alloc::sync::Arc; or use alloc::sync::{ Arc };
    
    type_patterns = {
        'Arc': r'use\s+alloc::sync::Arc\b',
        'Arc_block': r'use\s+alloc::sync::\{\s*Arc\s*\}',
        'Vec': r'use\s+alloc::vec::Vec\b',
        'Vec_block': r'use\s+alloc::vec::\{\s*Vec\s*\}',
        'Box': r'use\s+alloc::boxed::Box\b',
        'Box_block': r'use\s+alloc::boxed::\{\s*Box\s*\}',
        'String': r'use\s+alloc::string::String\b',
        'String_block': r'use\s+alloc::string::\{\s*String\s*\}',
        'VecDeque': r'use\s+alloc::collections::VecDeque\b',
        'VecDeque_block': r'use\s+alloc::collections::\{\s*VecDeque\s*\}',
        'BTreeMap': r'use\s+alloc::collections::BTreeMap\b',
        'BTreeMap_block': r'use\s+alloc::collections::\{\s*BTreeMap\s*\}',
    }

    for line in lines:
        stripped = line.strip()
        skip = False
        for t, p in type_patterns.items():
            base_t = t.split('_')[0]
            if re.search(p, stripped):
                if base_t in seen_types:
                    skip = True
                    break
                seen_types.add(base_t)
        
        if not skip:
            new_lines.append(line)
    
    content = '\n'.join(new_lines)
    
    # Also handle blocks like use alloc::sync::{ Arc, Mutex } where Arc might be duplicated
    # This is harder, but let's try a simpler approach for now.
    
    with open(path, 'w') as f:
        f.write(content)

def main():
    for root, dirs, files in os.walk('node/src'):
        for file in files:
            if file.endswith('.rs'):
                cleanup_duplicates(os.path.join(root, file))

if __name__ == '__main__':
    main()
