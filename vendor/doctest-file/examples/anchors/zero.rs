//{
#![allow(dead_code)]
use crate::Num;
//}
struct Zero;
impl Num for Zero {
    fn num(&self) -> u8 { 0 }
}
