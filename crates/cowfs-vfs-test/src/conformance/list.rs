/// The single list of checks. `$cb` names another exported macro that receives
/// `args ; [normal checks] [heavy checks]`.
#[macro_export]
#[doc(hidden)]
macro_rules! __conformance_checks {
    ($cb:ident $(, $arg:tt)*) => {
        $crate::$cb! { $($arg)* ;
            [
                (basic, root_is_directory),
                (basic, create_lookup_getattr),
                (basic, create_existing_is_exists),
                (basic, lookup_missing_is_not_found),
                (basic, lookup_in_file_is_not_dir),
                (basic, create_in_file_is_not_dir),
                (basic, create_masks_mode),
                (basic, new_file_attrs),
                (basic, distinct_files_distinct_inodes),
                (basic, dir_nlink_counts_subdirs),
                (basic, statfs_sane),
                (basic, stale_on_never_existing_inode),
                (basic, timestamps_track_wall_clock),
                (io, roundtrip_sizes),
                (io, read_past_eof_is_empty),
                (io, read_crossing_eof_is_short),
                (io, write_past_eof_makes_zero_hole),
                (io, overwrite_in_middle),
                (io, sparse_write_far_past_eof),
                (io, truncate_shrink_then_grow_zero_fills),
                (io, truncate_to_same_size),
                (io, write_updates_mtime_and_ctime),
                (io, blocks_accounting),
                (io, io_on_non_regular_files),
                (io, random_overlapping_writes_match_model),
                (attrs, setattr_mode_is_masked),
                (attrs, setattr_size_on_directory_is_isdir),
                (attrs, setattr_size_on_symlink_is_invalid),
                (attrs, setattr_failure_changes_nothing),
                (attrs, setattr_combined_changes_apply),
                (attrs, setattr_times_now_and_at),
                (attrs, setattr_bumps_ctime),
                (attrs, namespace_ops_update_times),
                (names, validate_name_cases),
                (names, name_max_ok_and_one_more_fails),
                (names, non_utf8_names),
                (names, names_are_exact_bytes),
                (names, invalid_names_rejected_by_creating_ops),
                (dirs, mkdir_rmdir_errors),
                (dirs, rmdir_updates_parent),
                (dirs, deeply_nested_directories),
                (dirs, create_in_removed_directory_fails),
                (readdir, readdir_empty_directory),
                (readdir, readdir_one_entry),
                (readdir, readdir_5000_entries),
                (readdir, readdir_never_lists_dot_entries),
                (readdir, readdir_cookies_resume_after_every_entry),
                (readdir, readdir_max_one),
                (readdir, readdir_eof_flag_is_exact),
                (readdir, readdir_cookies_distinct_and_nonzero),
                (readdir, readdir_stable_order),
                (readdir, readdir_entries_match_lookup),
                (readdir, readdir_on_file_is_not_dir),
                (readdir, readdir_delete_returned_entries_between_pages),
                (readdir, readdir_delete_upcoming_entries_between_pages),
                (readdir, readdir_delete_everything_between_pages),
                (readdir, readdir_add_entries_between_pages),
                (readdir, readdir_resume_from_removed_entry_cookie),
                (rename, rename_file_basic),
                (rename, rename_file_over_file),
                (rename, rename_file_over_empty_dir_is_isdir),
                (rename, rename_dir_over_empty_dir),
                (rename, rename_dir_over_non_empty_dir_is_not_empty),
                (rename, rename_dir_over_file_is_not_dir),
                (rename, rename_dir_into_own_subtree_is_invalid),
                (rename, rename_cross_directory),
                (rename, rename_dir_cross_directory_fixes_nlink),
                (rename, rename_onto_same_inode_is_noop),
                (rename, rename_no_replace),
                (rename, rename_missing_source_is_not_found),
                (rename, rename_open_file_keeps_handle_working),
                (rename, rename_updates_times),
                (rename, rename_dir_keeps_contents),
                (links, hardlink_shares_inode),
                (links, hardlink_nlink_counts_names),
                (links, hardlink_write_visible_through_other_name),
                (links, hardlink_unlink_one_other_survives),
                (links, hardlink_across_directories),
                (links, hardlink_to_directory_is_denied),
                (links, hardlink_over_existing_name_is_exists),
                (links, hardlink_to_symlink),
                (links, hardlink_pairs_8000_listed_once_and_removed),
                (lifecycle, unlink_while_open_keeps_data),
                (lifecycle, unlink_while_open_reclaimed_after_release_and_forget),
                (lifecycle, forget_keeps_inode_with_links),
                (lifecycle, forget_keeps_inode_with_handle),
                (lifecycle, rmdir_reclaimed_after_forget),
                (lifecycle, stale_after_reclaim_for_every_operation),
                (lifecycle, open_directory_ok),
                (lifecycle, two_handles_pin_until_last_release),
                (symlinks, symlink_create_and_readlink),
                (symlinks, symlink_dangling_ok),
                (symlinks, symlink_size_is_target_length),
                (symlinks, symlink_to_symlink),
                (symlinks, unlink_symlink_keeps_target),
                (symlinks, setattr_times_on_symlink_never_touch_target),
                (symlinks, readlink_on_non_symlink_is_invalid),
                (symlinks, symlink_over_existing_is_exists),
                (symlinks, symlink_binary_target),
                (readonly_mode, readonly_mode_0444_writes_always_succeed),
                (readonly_mode, readonly_mode_0400_writes_always_succeed),
                (xattrs, xattr_set_get_list_remove),
                (xattrs, xattr_create_and_replace_flags),
                (xattrs, xattr_missing_is_no_attr),
                (xattrs, xattr_empty_and_large_values),
                (xattrs, xattr_list_order_is_stable),
                (xattrs, xattr_on_directory_and_symlink),
                (xattrs, xattr_shared_by_hardlinks),
                (concurrency, concurrent_creates_in_one_directory),
                (concurrency, concurrent_writes_to_different_files),
                (concurrency, concurrent_readers_and_writers_of_one_file),
                (concurrency, concurrent_rename_unlink_lookup),
            ]
            [
                (io, roundtrip_8_mib),
                (readdir, readdir_50000_entries),
            ]
        }
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __registry {
    ( ; [ $(($m:ident, $n:ident)),* $(,)? ] [ $(($hm:ident, $hn:ident)),* $(,)? ] ) => {
        vec![
            $( $crate::conformance::Check {
                name: stringify!($n),
                category: stringify!($m),
                heavy: false,
                run: $crate::conformance::$m::$n,
            }, )*
            $( $crate::conformance::Check {
                name: stringify!($hn),
                category: stringify!($hm),
                heavy: true,
                run: $crate::conformance::$hm::$hn,
            }, )*
        ]
    };
}

#[macro_export]
#[doc(hidden)]
macro_rules! __gen_tests {
    ( $f:expr ; [ $(($m:ident, $n:ident)),* $(,)? ] [ $(($hm:ident, $hn:ident)),* $(,)? ] ) => {
        $(
            #[test]
            fn $n() {
                $crate::conformance::assert_named(stringify!($n), &$f);
            }
        )*
        $(
            #[test]
            #[ignore = "heavy: run with `cargo test -- --ignored`"]
            fn $hn() {
                $crate::conformance::assert_named(stringify!($hn), &$f);
            }
        )*
    };
}
