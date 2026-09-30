/// The single list of checks: `(module, name, Level)`. `$cb` names another exported macro that
/// receives `args ; [normal checks] [heavy checks]`.
#[macro_export]
#[doc(hidden)]
macro_rules! __conformance_checks {
    ($cb:ident $(, $arg:tt)*) => {
        $crate::$cb! { $($arg)* ;
            [
                (basic, root_is_directory, Cowfs),
                (basic, create_lookup_getattr, Posix),
                (basic, create_existing_is_exists, Posix),
                (basic, lookup_missing_is_not_found, Posix),
                (basic, lookup_in_file_is_not_dir, Portable),
                (basic, create_in_file_is_not_dir, Posix),
                (basic, create_masks_mode, Cowfs),
                (basic, new_file_attrs, Cowfs),
                (basic, distinct_files_distinct_inodes, Posix),
                (basic, dir_nlink_counts_subdirs, Cowfs),
                (basic, statfs_sane, Portable),
                (basic, statfs_free_after_unlink, Cowfs),
                (basic, stale_on_never_existing_inode, Cowfs),
                (basic, timestamps_track_wall_clock, Portable),
                (io, roundtrip_sizes, Posix),
                (io, read_past_eof_is_empty, Portable),
                (io, read_crossing_eof_is_short, Posix),
                (io, read_never_short_mid_file, Posix),
                (io, read_huge_size_on_small_file, Posix),
                (io, write_past_eof_makes_zero_hole, Posix),
                (io, overwrite_in_middle, Posix),
                (io, sparse_write_far_past_eof, Portable),
                (io, truncate_shrink_then_grow_zero_fills, Posix),
                (io, truncate_to_same_size, Posix),
                (io, truncate_to_same_size_bumps_ctime, Cowfs),
                (io, write_updates_mtime_and_ctime, Portable),
                (io, blocks_accounting, Posix),
                (io, io_on_non_regular_files, Cowfs),
                (io, random_overlapping_writes_match_model, Posix),
                (attrs, setattr_mode_is_masked, Portable),
                (attrs, setattr_size_on_directory_is_isdir, Portable),
                (attrs, setattr_size_on_symlink_is_invalid, Cowfs),
                (attrs, setattr_failure_changes_nothing, Cowfs),
                (attrs, setattr_combined_changes_apply, Cowfs),
                (attrs, setattr_times_now_and_at, Portable),
                (attrs, setattr_bumps_ctime, Portable),
                (attrs, namespace_ops_update_times, Portable),
                (names, validate_name_cases, Cowfs),
                (names, name_max_ok_and_one_more_fails, Posix),
                (names, non_utf8_names, Cowfs),
                (names, names_are_exact_bytes, Cowfs),
                (names, invalid_names_rejected_by_creating_ops, Cowfs),
                (names, appledouble_names_are_ordinary, Posix),
                (dirs, mkdir_rmdir_errors, Posix),
                (dirs, rmdir_updates_parent, Cowfs),
                (dirs, deeply_nested_directories, Cowfs),
                (dirs, create_in_removed_directory_fails, Cowfs),
                (readdir, readdir_empty_directory, Cowfs),
                (readdir, readdir_one_entry, Posix),
                (readdir, readdir_5000_entries, Posix),
                (readdir, readdir_never_lists_dot_entries, Cowfs),
                (readdir, readdir_cookies_resume_after_every_entry, Cowfs),
                (readdir, readdir_max_one, Cowfs),
                (readdir, readdir_max_zero_is_invalid, Cowfs),
                (readdir, readdir_eof_flag_is_exact, Cowfs),
                (readdir, readdir_cookies_distinct_and_nonzero, Cowfs),
                (readdir, readdir_stable_order, Cowfs),
                (readdir, readdir_entries_match_lookup, Posix),
                (readdir, readdir_on_file_is_not_dir, Posix),
                (readdir, readdir_delete_returned_entries_between_pages, Posix),
                (readdir, readdir_delete_upcoming_entries_between_pages, Posix),
                (readdir, readdir_delete_everything_between_pages, Posix),
                (readdir, readdir_add_entries_between_pages, Posix),
                (readdir, readdir_resume_from_removed_entry_cookie, Cowfs),
                (rename, rename_file_basic, Posix),
                (rename, rename_file_over_file, Posix),
                (rename, rename_file_over_empty_dir_is_isdir, Posix),
                (rename, rename_file_onto_ancestor_dir_is_rejected, Posix),
                (rename, rename_dir_over_empty_dir, Cowfs),
                (rename, rename_dir_over_non_empty_dir_is_not_empty, Posix),
                (rename, rename_dir_over_file_is_not_dir, Posix),
                (rename, rename_dir_into_own_subtree_is_invalid, Posix),
                (rename, rename_ancestor_into_moved_dir, Posix),
                (rename, rename_cross_directory, Posix),
                (rename, rename_dir_cross_directory_fixes_nlink, Cowfs),
                (rename, rename_dir_across_parents_keeps_parent_links, Cowfs),
                (rename, rename_onto_same_inode_is_noop, Portable),
                (rename, rename_no_replace, Portable),
                (rename, rename_missing_source_is_not_found, Posix),
                (rename, rename_open_file_keeps_handle_working, Cowfs),
                (rename, rename_updates_times, Portable),
                (rename, rename_dir_keeps_contents, Posix),
                (links, hardlink_shares_inode, Posix),
                (links, hardlink_nlink_counts_names, Cowfs),
                (links, lookup_nlink_is_fresh, Posix),
                (links, hardlink_write_visible_through_other_name, Posix),
                (links, hardlink_unlink_one_other_survives, Posix),
                (links, hardlink_across_directories, Posix),
                (links, hardlink_to_directory_is_denied, Posix),
                (links, hardlink_over_existing_name_is_exists, Posix),
                (links, hardlink_to_symlink, Portable),
                (links, hardlink_pairs_8000_listed_once_and_removed, Posix),
                (links, hardlink_limit_reports_too_many_links, Cowfs),
                (lifecycle, unlink_while_open_keeps_data, Cowfs),
                (lifecycle, unlink_while_open_reclaimed_after_release_and_forget, Cowfs),
                (lifecycle, forget_keeps_inode_with_links, Cowfs),
                (lifecycle, forget_keeps_inode_with_handle, Cowfs),
                (lifecycle, rmdir_reclaimed_after_forget, Cowfs),
                (lifecycle, stale_after_reclaim_for_every_operation, Cowfs),
                (lifecycle, open_directory_ok, Cowfs),
                (lifecycle, two_handles_pin_until_last_release, Cowfs),
                (symlinks, symlink_create_and_readlink, Posix),
                (symlinks, symlink_dangling_ok, Posix),
                (symlinks, symlink_size_is_target_length, Portable),
                (symlinks, symlink_size_multibyte, Portable),
                (symlinks, symlink_to_symlink, Posix),
                (symlinks, unlink_symlink_keeps_target, Posix),
                (symlinks, setattr_times_on_symlink_never_touch_target, Posix),
                (symlinks, readlink_on_non_symlink_is_invalid, Posix),
                (symlinks, symlink_over_existing_is_exists, Posix),
                (symlinks, symlink_binary_target, Portable),
                (readonly_mode, readonly_mode_0444_writes_always_succeed, Cowfs),
                (readonly_mode, readonly_mode_0400_writes_always_succeed, Cowfs),
                (xattrs, xattr_set_get_list_remove, Portable),
                (xattrs, xattr_create_and_replace_flags, Portable),
                (xattrs, xattr_missing_is_no_attr, Portable),
                (xattrs, xattr_empty_and_large_values, Cowfs),
                (xattrs, xattr_list_order_is_stable, Portable),
                (xattrs, xattr_on_directory_and_symlink, Cowfs),
                (xattrs, xattr_shared_by_hardlinks, Portable),
                (xattrs, xattr_name_validation, Cowfs),
                (concurrency, concurrent_creates_in_one_directory, Posix),
                (concurrency, concurrent_writes_to_different_files, Posix),
                (concurrency, concurrent_readers_and_writers_of_one_file, Portable),
                (concurrency, concurrent_rename_unlink_lookup, Cowfs),
            ]
            [
                (io, roundtrip_8_mib, Posix),
                (readdir, readdir_50000_entries, Posix),
            ]
        }
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __registry {
    ( ; [ $(($m:ident, $n:ident, $l:ident)),* $(,)? ] [ $(($hm:ident, $hn:ident, $hl:ident)),* $(,)? ] ) => {
        vec![
            $( $crate::conformance::Check {
                name: stringify!($n),
                category: stringify!($m),
                level: $crate::conformance::Level::$l,
                heavy: false,
                run: $crate::conformance::$m::$n,
            }, )*
            $( $crate::conformance::Check {
                name: stringify!($hn),
                category: stringify!($hm),
                level: $crate::conformance::Level::$hl,
                heavy: true,
                run: $crate::conformance::$hm::$hn,
            }, )*
        ]
    };
}

/// One `#[test]` per check. A check named in the skip list becomes an `#[ignore = "reason"]`
/// test, so libtest prints it as ignored with the reason; running ignored tests only prints a
/// note for it. `$d` is a `$` token, needed to write the inner `macro_rules!`.
#[macro_export]
#[doc(hidden)]
macro_rules! __gen_tests {
    ( ($d:tt) { $($sn:ident => $sr:literal),* } $f:expr ;
      [ $(($m:ident, $n:ident, $l:ident)),* $(,)? ]
      [ $(($hm:ident, $hn:ident, $hl:ident)),* $(,)? ] ) => {
        macro_rules! __cowfs_gate {
            $(
                ($sn, $d kind:tt, $d ($d item:tt)*) => {
                    #[test]
                    #[ignore = $sr]
                    fn $sn() {
                        eprintln!("conformance check {} skipped: {}", stringify!($sn), $sr);
                    }
                };
            )*
            ($d name:ident, normal, $d ($d item:tt)*) => {
                #[test]
                $d ($d item)*
            };
            ($d name:ident, heavy, $d ($d item:tt)*) => {
                #[test]
                #[ignore = "heavy: run with COWFS_CONFORMANCE_HEAVY=1 cargo test -- --ignored"]
                $d ($d item)*
            };
        }
        $(
            __cowfs_gate!($n, normal, fn $n() {
                $crate::conformance::assert_named(stringify!($n), &$f);
            });
        )*
        $(
            __cowfs_gate!($hn, heavy, fn $hn() {
                $crate::conformance::assert_named(stringify!($hn), &$f);
            });
        )*
        #[test]
        fn conformance_skip_names_exist() {
            $crate::conformance::assert_skip_names(&[$(stringify!($sn)),*]);
        }
    };
}
