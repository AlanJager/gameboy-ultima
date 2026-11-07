use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub(crate) fn serialize<T, S, const N: usize>(
    values: &[T; N],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    values.as_slice().serialize(serializer)
}

pub(crate) fn deserialize<'de, T, D, const N: usize>(deserializer: D) -> Result<[T; N], D::Error>
where
    T: Copy + Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    let values = Vec::<T>::deserialize(deserializer)?;
    if values.len() != N {
        return Err(serde::de::Error::invalid_length(
            values.len(),
            &"the correct number of elements",
        ));
    }
    let mut array = [T::default(); N];
    array.copy_from_slice(&values);
    Ok(array)
}
