// Copyright (C) 2019-2023 Aleo Systems Inc.
// This file is part of the snarkVM library.

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at:
// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod helpers;
pub use helpers::*;

mod path;
pub use path::*;

#[cfg(test)]
mod tests;

use snarkvm_console_types::prelude::*;

use aleo_std::prelude::*;

#[derive(Clone)]
pub struct KaryMerkleTree<LH: LeafHash<Hash = PH::Hash>, PH: PathHash, const DEPTH: u8, const ARITY: u8> {
    /// The leaf hasher for the Merkle tree.
    leaf_hasher: LH,
    /// The path hasher for the Merkle tree.
    path_hasher: PH,
    /// The computed root of the full Merkle tree.
    root: PH::Hash,
    /// The internal hashes, from root to hashed leaves, of the full Merkle tree.
    tree: Vec<PH::Hash>,
    /// The canonical empty hash.
    empty_hash: PH::Hash,
    /// The number of hashed leaves in the tree.
    number_of_leaves: usize,
}

/// Returns the next power of `n` that's greater than or equal to `base`.
/// Returns `None` for edge cases or in case of overflow.
fn checked_next_power_of_n(base: usize, n: usize) -> Option<usize> {
    if n <= 1 {
        return None;
    }

    let mut value = 1;
    while value < base {
        value = value.checked_mul(n)?;
    }
    Some(value)
}

impl<LH: LeafHash<Hash = PH::Hash>, PH: PathHash, const DEPTH: u8, const ARITY: u8>
    KaryMerkleTree<LH, PH, DEPTH, ARITY>
{
    /// Initializes a new Merkle tree with the given leaves.
    #[inline]
    pub fn new(leaf_hasher: &LH, path_hasher: &PH, leaves: &[LH::Leaf]) -> Result<Self> {
        let timer = timer!("MerkleTree::new");

        // Ensure the Merkle tree depth is greater than 0.
        ensure!(DEPTH > 0, "Merkle tree depth must be greater than 0");
        // Ensure the Merkle tree depth is less than or equal to 64.
        ensure!(DEPTH <= 64u8, "Merkle tree depth must be less than or equal to 64");
        // Ensure the Merkle tree arity is greater than 1.
        ensure!(ARITY > 1, "Merkle tree arity must be greater than 1");
        // Ensure the Merkle tree does not overflow a u128.
        ensure!((ARITY as u128).checked_pow(DEPTH as u32).is_some(), "Merkle tree size overflowed");

        // Compute the maximum number of leaves.
        let Some(max_leaves) = checked_next_power_of_n(leaves.len(), ARITY as usize) else {
            bail!("Integer overflow when computing the maximum number of leaves in the Merkle tree");
        };

        // Compute the number of nodes.
        let num_nodes = (max_leaves - 1) / (ARITY as usize - 1);
        // Compute the tree size as the maximum number of leaves plus the number of nodes.
        let tree_size = max_leaves + num_nodes;
        // Compute the number of levels in the Merkle tree (i.e. log_arity(tree_size)).
        let tree_depth = tree_depth::<DEPTH, ARITY>(tree_size)?;
        // Compute the number of padded levels.
        let padding_depth = DEPTH - tree_depth;

        // Compute the empty hash.
        let empty_hash = path_hasher.hash_empty::<ARITY>()?;

        // Calculate the size of the tree which excludes leafless nodes.
        let arity = ARITY as usize;
        let minimum_tree_size = std::cmp::max(
            1,
            num_nodes + leaves.len() + if leaves.len() % arity == 0 { 0 } else { arity - leaves.len() % arity },
        );

        // Prepare a cache of hashes of empty nodes and their derivatives.
        let mut empty_hash_cache = Vec::with_capacity(DEPTH as usize);
        empty_hash_cache.push(path_hasher.hash_children(&vec![empty_hash; arity])?);
        for i in 0..(DEPTH as usize - 1) {
            empty_hash_cache.push(path_hasher.hash_children(&vec![empty_hash_cache[i]; arity])?);
        }

        // Initialize the Merkle tree.
        let mut tree = vec![empty_hash; minimum_tree_size];

        // Compute and store each leaf hash.
        tree[num_nodes..num_nodes + leaves.len()].clone_from_slice(&leaf_hasher.hash_leaves(leaves)?);
        lap!(timer, "Hashed {} leaves", leaves.len());

        // Compute and store the hashes for each level, iterating from the penultimate level to the root level.
        let mut start_index = num_nodes;
        let mut current_level = 0;
        // Compute the start index of the current level.
        while let Some(start) = parent::<ARITY>(start_index) {
            // Compute the end index of the current level.
            let end = child_indexes::<ARITY>(start).next().ok_or_else(|| anyhow!("Missing left-most child"))?;

            // Construct the children for each node in the current level.
            let child_nodes = (start..end)
                .map(|i| {
                    // Prepare an iterator over then children, being mindful of possible missing leaves.
                    let child_iter = || child_indexes::<ARITY>(i).map(|child_index| tree.get(child_index).copied());

                    // At the leaf level just return the leaves, or `None` in case the node has none.
                    if current_level == 0 {
                        return child_iter().collect::<Option<Vec<_>>>();
                    }

                    // Check if the children aren't all derived from empty hashes.
                    let current_composite_empty_hash = empty_hash_cache[current_level - 1];
                    if child_iter().all(|hash| hash == Some(current_composite_empty_hash)) {
                        return None;
                    }

                    // Collect the children.
                    child_iter().collect::<Option<Vec<_>>>()
                })
                .collect::<Vec<_>>();

            // Compute and store the hashes for each node in the current level.
            tree[start..end].clone_from_slice(
                &path_hasher.hash_all_children::<ARITY>(&child_nodes, empty_hash_cache[current_level])?,
            );
            // Update the start index for the next level.
            start_index = start;
            current_level += 1;
        }
        lap!(timer, "Hashed {} levels", tree_depth);

        // Compute the root hash, by iterating from the root level up to `DEPTH`.
        let mut root_hash = tree[0];
        for _ in 0..padding_depth {
            // Update the root hash, by hashing the current root hash with the empty hashes.

            let mut input = Vec::with_capacity(ARITY as usize);
            input.push(root_hash);
            // Resize the vector to ARITY length, filling with empty_hash if necessary.
            input.resize(ARITY as usize, empty_hash);

            root_hash = path_hasher.hash_children(&input)?;
        }
        lap!(timer, "Hashed {} padding levels", padding_depth);

        finish!(timer);

        Ok(Self {
            leaf_hasher: leaf_hasher.clone(),
            path_hasher: path_hasher.clone(),
            root: root_hash,
            tree,
            empty_hash,
            number_of_leaves: leaves.len(),
        })
    }

    #[inline]
    /// Returns the Merkle path for the given leaf index and leaf.
    pub fn prove(&self, leaf_index: usize, leaf: &LH::Leaf) -> Result<KaryMerklePath<PH, DEPTH, ARITY>> {
        // Ensure the leaf index is valid.
        ensure!(leaf_index < self.number_of_leaves, "The given Merkle leaf index is out of bounds");

        // Compute the leaf hash.
        let leaf_hash = self.leaf_hasher.hash_leaf(leaf)?;

        // Compute the start index (on the left) for the leaf hashes level in the Merkle tree.
        let start = match checked_next_power_of_n(self.number_of_leaves, ARITY as usize) {
            Some(num_leaves) => (num_leaves - 1) / (ARITY as usize - 1),
            None => bail!("Integer overflow when computing the Merkle tree start index"),
        };

        // Compute the absolute index of the leaf in the Merkle tree.
        let mut index = start + leaf_index;
        // Ensure the leaf index is valid.
        ensure!(index < self.tree.len(), "The given Merkle leaf index is out of bounds");
        // Ensure the leaf hash matches the one in the tree.
        ensure!(self.tree[index] == leaf_hash, "The given Merkle leaf does not match the one in the Merkle tree");

        // Initialize a vector for the Merkle path.
        let mut path = Vec::with_capacity(DEPTH as usize);

        // Iterate from the leaf hash to the root level, storing the sibling hashes along the path.
        for _ in 0..DEPTH {
            // Compute the index of the sibling hash, if it exists.
            if let Some(siblings) = siblings::<ARITY>(index) {
                // Append the sibling hashes to the path.
                let sibling_hashes = siblings.map(|index| self.tree[index]).collect::<Vec<_>>();

                path.push(sibling_hashes);
                // Compute the index of the parent hash, if it exists.
                match parent::<ARITY>(index) {
                    // Update the index to the parent index.
                    Some(parent) => index = parent,
                    // If the parent does not exist, the path is complete.
                    None => break,
                }
            }
        }

        // If the Merkle path length is not equal to `DEPTH`, pad the path with the empty hash.
        if path.len() != DEPTH as usize {
            let empty_hashes = (0..ARITY.saturating_sub(1)).map(|_| self.empty_hash).collect::<Vec<_>>();
            path.resize(DEPTH as usize, empty_hashes);
        }

        // Return the Merkle path.
        KaryMerklePath::try_from((leaf_index as u64, path))
    }

    /// Returns `true` if the given Merkle path is valid for the given root and leaf.
    pub fn verify(&self, path: &KaryMerklePath<PH, DEPTH, ARITY>, root: &PH::Hash, leaf: &LH::Leaf) -> bool {
        path.verify(&self.leaf_hasher, &self.path_hasher, root, leaf)
    }

    /// Returns the Merkle root of the tree.
    pub const fn root(&self) -> &PH::Hash {
        &self.root
    }

    /// Returns the Merkle tree (excluding the hashes of the leaves).
    pub fn tree(&self) -> &[PH::Hash] {
        &self.tree
    }

    /// Returns the empty hash.
    pub const fn empty_hash(&self) -> &PH::Hash {
        &self.empty_hash
    }

    /// Returns the number of leaves in the Merkle tree.
    pub const fn number_of_leaves(&self) -> usize {
        self.number_of_leaves
    }
}

/// Returns the depth of the tree, given the size of the tree.
#[inline]
#[allow(clippy::cast_possible_truncation)]
fn tree_depth<const DEPTH: u8, const ARITY: u8>(tree_size: usize) -> Result<u8> {
    let tree_size = u64::try_from(tree_size)?;

    ensure!(tree_size < 4503599627370496_u64, "Tree size must be less than 2^52");

    // Calculate the tree depth based on the tree size and arity.
    // log_arity(tree_size) =  ln(tree_size) / ln(arity).
    let tree_depth_float = (tree_size as f64).ln() / (ARITY as f64).ln();
    let tree_depth = u8::try_from(tree_depth_float.floor() as u64)?;

    // Ensure the tree depth is within the depth bound.
    match tree_depth <= DEPTH {
        // Return the tree depth.
        true => Ok(tree_depth),
        false => bail!("Merkle tree cannot exceed depth {DEPTH}: attempted to reach depth {tree_depth}"),
    }
}

/// Returns the indexes of the children, given an index.
fn child_indexes<const ARITY: u8>(index: usize) -> impl Iterator<Item = usize> {
    let start = index * ARITY as usize + 1;
    start..start + ARITY as usize
}

/// Returns the index of the siblings, given an index.
#[inline]
fn siblings<const ARITY: u8>(index: usize) -> Option<impl Iterator<Item = usize>> {
    if is_root(index) {
        None
    } else {
        // Find the left-most sibling.
        let left_most_sibling = ((index - 1) / ARITY as usize) * ARITY as usize + 1;

        // Add all the siblings except for the given index.
        Some((left_most_sibling..left_most_sibling + ARITY as usize).filter(move |&i| index != i))
    }
}

/// Returns true iff the index represents the root.
#[inline]
const fn is_root(index: usize) -> bool {
    index == 0
}

/// Returns the index of the parent, given the index of a child.
#[inline]
const fn parent<const ARITY: u8>(index: usize) -> Option<usize> {
    if index > 0 { Some((index - 1) / ARITY as usize) } else { None }
}
